//! Sanitize proxy responses so secrets never reach chat / agents / logs.

use serde_json::{json, Map, Value};

use super::types::MAX_PROXY_DATA_BYTES;

const SECRET_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "token",
    "client_secret",
    "password",
    "authorization",
    "api_key",
    "apikey",
    "secret",
    "private_key",
];

/// Recursively strip secret-like keys and cap payload size.
pub fn sanitize_value(v: &Value) -> Value {
    let cleaned = strip_secrets(v);
    cap_value_size(cleaned, MAX_PROXY_DATA_BYTES)
}

fn strip_secrets(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, val) in map {
                if is_secret_key(k) {
                    continue;
                }
                out.insert(k.clone(), strip_secrets(val));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(strip_secrets).collect()),
        other => other.clone(),
    }
}

fn is_secret_key(k: &str) -> bool {
    let lower = k.to_ascii_lowercase().replace('-', "_");
    if SECRET_KEYS.iter().any(|s| lower == *s) {
        return true;
    }
    // Suffixes only — `contains("token")` would strip Google Drive nextPageToken.
    const SUFFIXES: &[&str] = &[
        "_access_token",
        "_refresh_token",
        "_id_token",
        "_client_secret",
        "_api_key",
        "_private_key",
        "_authorization",
    ];
    SUFFIXES.iter().any(|s| lower.ends_with(s))
}

fn cap_value_size(v: Value, max_bytes: usize) -> Value {
    match serde_json::to_vec(&v) {
        Ok(bytes) if bytes.len() <= max_bytes => v,
        Ok(bytes) => {
            // Truncate arrays first when possible.
            if let Value::Object(mut map) = v
                && let Some(Value::Array(items)) = map.get("items").cloned() {
                    let mut kept = Vec::new();
                    for item in items {
                        kept.push(item);
                        let candidate = {
                            let mut m = map.clone();
                            m.insert("items".into(), Value::Array(kept.clone()));
                            m.insert("truncated".into(), json!(true));
                            Value::Object(m)
                        };
                        if serde_json::to_vec(&candidate)
                            .map(|b| b.len() <= max_bytes)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        kept.pop();
                        break;
                    }
                    map.insert("items".into(), Value::Array(kept));
                    map.insert("truncated".into(), json!(true));
                    return Value::Object(map);
                }
            json!({
                "truncated": true,
                "note": format!(
                    "response exceeded {max_bytes} bytes (was {}); request a narrower query",
                    bytes.len()
                ),
            })
        }
        Err(_) => json!({ "error": "failed to serialize proxy data" }),
    }
}

/// True if the value tree still contains secret-like keys (for tests).
pub fn contains_secret_keys(v: &Value) -> bool {
    match v {
        Value::Object(map) => map
            .iter()
            .any(|(k, val)| is_secret_key(k) || contains_secret_keys(val)),
        Value::Array(arr) => arr.iter().any(contains_secret_keys),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_pagination_tokens() {
        let raw = json!({
            "nextPageToken": "abc",
            "pageToken": "def",
            "access_token": "nope"
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["nextPageToken"], "abc");
        assert_eq!(clean["pageToken"], "def");
        assert!(clean.get("access_token").is_none());
    }

    #[test]
    fn strips_tokens() {
        let raw = json!({
            "title": "ok",
            "access_token": "secret",
            "nested": { "refresh_token": "x", "name": "y" }
        });
        let clean = sanitize_value(&raw);
        assert!(!contains_secret_keys(&clean));
        assert_eq!(clean["title"], "ok");
        assert_eq!(clean["nested"]["name"], "y");
    }

    #[test]
    fn strips_secret_keys_from_sample_json() {
        let raw = json!({
            "id": 1,
            "Authorization": "Bearer leak",
            "api-key": "k",
            "client_secret": "cs",
            "password": "p",
            "private_key": "pk",
            "token": "t",
            "items": [
                { "name": "keep", "github_access_token": "gat", "secret": "s" }
            ]
        });
        assert!(contains_secret_keys(&raw));
        let clean = sanitize_value(&raw);
        assert!(!contains_secret_keys(&clean));
        assert_eq!(clean["id"], 1);
        assert_eq!(clean["items"][0]["name"], "keep");
        for key in [
            "Authorization",
            "api-key",
            "client_secret",
            "password",
            "private_key",
            "token",
        ] {
            assert!(clean.get(key).is_none(), "secret key `{key}` should be stripped");
        }
        assert!(clean["items"][0].get("github_access_token").is_none());
        assert!(clean["items"][0].get("secret").is_none());
    }

    #[test]
    fn caps_large_item_lists() {
        let items: Vec<Value> = (0..5000)
            .map(|i| json!({ "id": i, "title": format!("item-{i}-{}", "x".repeat(40)) }))
            .collect();
        let raw = json!({ "items": items, "count": 5000 });
        let clean = sanitize_value(&raw);
        let bytes = serde_json::to_vec(&clean).unwrap();
        assert!(bytes.len() <= MAX_PROXY_DATA_BYTES + 256);
    }
}
