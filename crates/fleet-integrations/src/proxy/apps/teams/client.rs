//! Microsoft Graph helpers for Teams (control plane only).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

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

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
}

pub(crate) async fn graph_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn graph_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match headers(http.post(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "teams",
        op,
        code,
        format!("Microsoft Graph {status}: {msg}"),
        Some(status),
    )
}

pub(crate) fn map_chat(c: &Value) -> Value {
    json!({
        "id": c.get("id"),
        "topic": c.get("topic"),
        "chatType": c.get("chatType"),
        "webUrl": c.get("webUrl"),
        "lastUpdatedDateTime": c.get("lastUpdatedDateTime"),
    })
}

pub(crate) fn map_team(t: &Value) -> Value {
    json!({
        "id": t.get("id"),
        "displayName": t.get("displayName"),
        "description": t.get("description"),
    })
}

pub(crate) fn map_channel(c: &Value) -> Value {
    json!({
        "id": c.get("id"),
        "displayName": c.get("displayName"),
        "description": c.get("description"),
        "membershipType": c.get("membershipType"),
        "webUrl": c.get("webUrl"),
    })
}

pub(crate) fn map_message(m: &Value) -> Value {
    let body = m
        .get("body")
        .and_then(|b| b.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "id": m.get("id"),
        "createdDateTime": m.get("createdDateTime"),
        "from": m.get("from")
            .and_then(|f| f.get("user"))
            .and_then(|u| u.get("displayName")),
        "content": truncate_str(body, 1500),
        "webUrl": m.get("webUrl"),
    })
}
