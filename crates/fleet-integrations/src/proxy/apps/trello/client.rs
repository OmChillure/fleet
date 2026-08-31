//! Trello HTTP helpers (key + user token).

use serde_json::{Value, json};

use crate::proxy::types::ProxyResult;

const API: &str = "https://api.trello.com/1";

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} is required"))
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

pub(crate) fn map_board(b: &Value) -> Value {
    json!({
        "id": b.get("id"),
        "name": b.get("name"),
        "url": b.get("url"),
        "closed": b.get("closed"),
        "idOrganization": b.get("idOrganization"),
    })
}

pub(crate) fn map_list(l: &Value) -> Value {
    json!({
        "id": l.get("id"),
        "name": l.get("name"),
        "closed": l.get("closed"),
        "idBoard": l.get("idBoard"),
        "pos": l.get("pos"),
    })
}

pub(crate) fn map_card(c: &Value) -> Value {
    json!({
        "id": c.get("id"),
        "name": c.get("name"),
        "desc": truncate_str(c.get("desc").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "url": c.get("url").or_else(|| c.get("shortUrl")),
        "idBoard": c.get("idBoard"),
        "idList": c.get("idList"),
        "closed": c.get("closed"),
        "due": c.get("due"),
        "labels": c.get("labels").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
        }),
        "dateLastActivity": c.get("dateLastActivity"),
    })
}

fn with_auth(url: &str, api_key: &str, token: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!(
        "{url}{sep}key={}&token={}",
        urlencoding(api_key),
        urlencoding(token)
    )
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) async fn trello_get(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    path: &str,
) -> (u16, Value) {
    let url = with_auth(&format!("{API}{path}"), api_key, token);
    match http
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn trello_post(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let url = with_auth(&format!("{API}{path}"), api_key, token);
    match http
        .post(&url)
        .header("Accept", "application/json")
        .json(body)
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn trello_put(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let url = with_auth(&format!("{API}{path}"), api_key, token);
    match http
        .put(&url)
        .header("Accept", "application/json")
        .json(body)
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn missing_key(op: &str) -> ProxyResult {
    ProxyResult::err(
        "trello",
        op,
        "token_missing",
        "Trello connection is missing api_key — reconnect the integration",
        None,
    )
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            body.get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            body.get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "upstream error".into());
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "trello",
        op,
        code,
        format!("Trello API {status}: {msg}"),
        Some(status),
    )
}
