//! Notion REST helpers (control plane only).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

const NOTION_VERSION: &str = "2022-06-28";

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} is required"))
}

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", NOTION_VERSION)
        .header("Content-Type", "application/json")
}

pub(crate) async fn notion_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn notion_post(
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

pub(crate) async fn notion_patch(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match headers(http.patch(url).json(body), token).send().await {
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
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.pointer("/error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "notion",
        op,
        code,
        format!("Notion API {status}: {msg}"),
        Some(status),
    )
}

pub(crate) fn map_search_item(item: &Value) -> Value {
    let object = item.get("object").and_then(|v| v.as_str()).unwrap_or("");
    let title = extract_title(item);
    json!({
        "id": item.get("id"),
        "object": object,
        "url": item.get("url"),
        "title": title,
        "last_edited_time": item.get("last_edited_time"),
        "parent": item.get("parent"),
    })
}

fn extract_title(item: &Value) -> Value {
    if let Some(title) = item.pointer("/properties/title/title")
        && let Some(arr) = title.as_array()
    {
        let s: String = arr
            .iter()
            .filter_map(|t| t.pointer("/plain_text").and_then(|v| v.as_str()))
            .collect();
        if !s.is_empty() {
            return json!(s);
        }
    }
    if let Some(title) = item.pointer("/properties/Name/title").or_else(|| item.get("title")) {
        if let Some(arr) = title.as_array() {
            let s: String = arr
                .iter()
                .filter_map(|t| {
                    t.pointer("/plain_text")
                        .or_else(|| t.pointer("/text/content"))
                        .and_then(|v| v.as_str())
                })
                .collect();
            if !s.is_empty() {
                return json!(s);
            }
        }
    }
    Value::Null
}

pub(crate) fn rich_text_paragraph(text: &str) -> Value {
    json!({
        "object": "block",
        "type": "paragraph",
        "paragraph": {
            "rich_text": [{
                "type": "text",
                "text": { "content": text.chars().take(2000).collect::<String>() }
            }]
        }
    })
}
