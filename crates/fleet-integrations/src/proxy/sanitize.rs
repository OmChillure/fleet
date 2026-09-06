//! Sanitize proxy responses so secrets never reach chat / agents / logs.

use serde_json::{json, Map, Value};

use super::types::MAX_PROXY_DATA_BYTES;

const SECRET_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    // camelCase OIDC (`idToken`) lowercases to this, not `id_token`.
    "idtoken",
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
                && let Some(Value::Array(items)) = map.get("items").cloned()
            {
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

    /// Nested-object `refresh_token` is dropped; `nested.name` stays.
    #[test]
    fn strips_tokens() {
        const ACCESS: &str = "gho_SANITIZE_MUST_NOT_ECHO_aa11";
        const REFRESH: &str = "refresh_SANITIZE_NESTED_x";
        let raw = json!({
            "title": "ok",
            "access_token": ACCESS,
            "nested": { "refresh_token": REFRESH, "name": "y" }
        });
        let clean = sanitize_value(&raw);
        assert!(
            clean.get("access_token").is_none(),
            "top-level access_token must not reach chat: {clean}"
        );
        assert!(
            clean["nested"].get("refresh_token").is_none(),
            "nested refresh_token must not reach chat: {clean}"
        );
        assert_eq!(clean["title"], "ok");
        assert_eq!(clean["nested"]["name"], "y");
        let clean_s = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !clean_s.contains(ACCESS) && !clean_s.contains(REFRESH),
            "sanitized body must not echo token values: {clean_s}"
        );
        assert!(!contains_secret_keys(&clean));
    }

    /// Drive-style nested pagination keys stay; nested `access_token` is dropped.
    #[test]
    fn keeps_nested_pagination_tokens() {
        const ACCESS: &str = "gho_NESTED_PAGE_MUST_NOT_ECHO";
        let raw = json!({
            "files": {
                "nextPageToken": "abc",
                "pageToken": "def",
                "access_token": ACCESS
            }
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["files"]["nextPageToken"], "abc");
        assert_eq!(clean["files"]["pageToken"], "def");
        assert!(
            clean["files"].get("access_token").is_none(),
            "nested access_token must not reach chat: {clean}"
        );
        let clean_s = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !clean_s.contains(ACCESS),
            "sanitized body must not echo token values: {clean_s}"
        );
    }

    /// camelCase `apiKey` lowercases to `apikey`, not hyphen-normalized `api_key`.
    #[test]
    fn strips_camelcase_api_key() {
        let raw = json!({
            "name": "keep",
            "apiKey": "camel-api-secret",
            "ApiKey": "pascal-api-secret"
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["name"], "keep");
        for key in ["apiKey", "ApiKey"] {
            assert!(
                clean.get(key).is_none(),
                "camelCase apiKey `{key}` must not reach chat: {clean}"
            );
        }
        let clean_s = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !clean_s.contains("camel-api-secret") && !clean_s.contains("pascal-api-secret"),
            "sanitized body must not echo apiKey values: {clean_s}"
        );
    }

    /// Suffix `_refresh_token` must strip provider-prefixed refresh tokens.
    #[test]
    fn strips_suffixed_refresh_token() {
        const GOOGLE: &str = "refresh_GOOGLE_MUST_NOT_ECHO";
        const GITHUB: &str = "refresh_GITHUB_MUST_NOT_ECHO";
        let raw = json!({
            "ok": 1,
            "google_refresh_token": GOOGLE,
            "github_refresh_token": GITHUB
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["ok"], 1);
        for key in ["google_refresh_token", "github_refresh_token"] {
            assert!(
                clean.get(key).is_none(),
                "suffixed refresh_token `{key}` must not reach chat: {clean}"
            );
        }
        let clean_s = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !clean_s.contains(GOOGLE) && !clean_s.contains(GITHUB),
            "sanitized body must not echo refresh token values: {clean_s}"
        );
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
            assert!(
                clean.get(key).is_none(),
                "secret key `{key}` should be stripped"
            );
        }
        assert!(clean["items"][0].get("github_access_token").is_none());
        assert!(clean["items"][0].get("secret").is_none());
    }

    /// OIDC `id_token` is a JWT. Suffix `_id_token` does not match the bare key.
    #[test]
    fn strips_bare_oidc_id_token() {
        let raw = json!({
            "user": "ok",
            "id_token": "eyJhbGciOiJIUzI1NiJ9.e30.sig",
            "Id-Token": "also-a-jwt",
            "google_id_token": "nested-suffix-jwt"
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["user"], "ok");
        for key in ["id_token", "Id-Token", "google_id_token"] {
            assert!(
                clean.get(key).is_none(),
                "OIDC id_token `{key}` must not reach chat: {clean}"
            );
        }
        assert!(!contains_secret_keys(&clean));
    }

    /// Google/OIDC camelCase `idToken` lowercases to `idtoken`, not `id_token`.
    #[test]
    fn strips_camelcase_oidc_id_token() {
        let raw = json!({
            "user": "ok",
            "idToken": "eyJhbGciOiJIUzI1NiJ9.e30.sig",
            "IdToken": "also-a-jwt",
            "nextPageToken": "abc",
            "pageToken": "def"
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["user"], "ok");
        assert_eq!(clean["nextPageToken"], "abc");
        assert_eq!(clean["pageToken"], "def");
        for key in ["idToken", "IdToken"] {
            assert!(
                clean.get(key).is_none(),
                "OIDC camelCase idToken `{key}` must not reach chat: {clean}"
            );
        }
        assert!(!contains_secret_keys(&clean));
    }

    /// Huge provider `items` lists shrink to a prefix under 64KiB and keep `count`.
    #[test]
    fn caps_large_item_lists() {
        let items: Vec<Value> = (0..5000)
            .map(|i| json!({ "id": i, "title": format!("item-{i}-{}", "x".repeat(40)) }))
            .collect();
        let raw = json!({ "items": items, "count": 5000 });
        let clean = sanitize_value(&raw);
        let bytes = serde_json::to_vec(&clean).expect("serialize");
        assert!(
            bytes.len() <= MAX_PROXY_DATA_BYTES,
            "sanitized proxy data must fit 64KiB: {} > {MAX_PROXY_DATA_BYTES}",
            bytes.len()
        );
        assert_eq!(
            clean["truncated"],
            true,
            "over-max items list must set truncated"
        );
        assert_eq!(
            clean["count"],
            5000,
            "items-prefix shrink must keep the original count"
        );
        let kept = clean["items"].as_array().expect("items array");
        let original = raw["items"].as_array().expect("fixture items");
        assert!(
            !kept.is_empty(),
            "prefix shrink must keep some items, not the note-only fallback"
        );
        assert!(
            kept.len() < 5000,
            "5000 oversized titles must not all reach chat: {}",
            kept.len()
        );
        assert_eq!(
            &kept[..],
            &original[..kept.len()],
            "kept items must be the original prefix"
        );
        assert!(
            clean.get("note").is_none(),
            "items-prefix path must not swap in the note fallback"
        );
    }

    /// Over-max object without an `items` array uses the note fallback, not prefix shrink.
    #[test]
    fn oversized_object_without_items_returns_truncated_note() {
        let raw = json!({ "content": "x".repeat(70_000) });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["truncated"], true);
        let note = clean["note"]
            .as_str()
            .expect("fallback note must be a string");
        assert!(
            note.contains(&MAX_PROXY_DATA_BYTES.to_string()),
            "fallback note must name the 64KiB cap: {note}"
        );
        assert!(
            clean.get("content").is_none(),
            "oversize content must not reach chat: {clean}"
        );
        let bytes = serde_json::to_vec(&clean).expect("serialize");
        assert!(
            bytes.len() <= MAX_PROXY_DATA_BYTES,
            "fallback note must itself fit the cap: {}",
            bytes.len()
        );
    }

    /// Cap must not fire on list_prs-sized payloads.
    #[test]
    fn items_under_max_are_not_marked_truncated() {
        let raw = json!({
            "items": [
                { "id": 1, "title": "first" },
                { "id": 2, "title": "second" }
            ]
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["items"][0], json!({ "id": 1, "title": "first" }));
        assert_eq!(clean["items"][1], json!({ "id": 2, "title": "second" }));
        assert_eq!(clean["items"].as_array().map(Vec::len), Some(2));
        assert!(
            clean.get("truncated").is_none(),
            "small items list must not be marked truncated: {clean}"
        );
        let in_bytes = serde_json::to_vec(&raw).expect("serialize input");
        let out_bytes = serde_json::to_vec(&clean).expect("serialize");
        assert_eq!(
            out_bytes.len(),
            in_bytes.len(),
            "under-max items must keep the input byte length"
        );
    }

    /// Prefix loop pops the only item that cannot fit; a note-only fallback would drop count.
    #[test]
    fn single_item_over_max_returns_empty_items_and_truncated() {
        let raw = json!({
            "items": [{ "blob": "x".repeat(70_000) }],
            "count": 1
        });
        let clean = sanitize_value(&raw);
        assert_eq!(clean["items"], json!([]));
        assert_eq!(clean["truncated"], true);
        assert_eq!(clean["count"], 1);
        let bytes = serde_json::to_vec(&clean).expect("serialize");
        assert!(
            bytes.len() <= MAX_PROXY_DATA_BYTES,
            "empty-items shrink must fit the cap: {}",
            bytes.len()
        );
        assert!(
            clean.get("note").is_none(),
            "items path must keep count, not replace the object with a note: {clean}"
        );
    }
}
