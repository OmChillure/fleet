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

pub(crate) fn notion_api_origin() -> String {
    #[cfg(test)]
    if let Some(base) = TEST_NOTION_API_BASE.with(|c| c.borrow().clone()) {
        return base;
    }
    "https://api.notion.com".into()
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_NOTION_API_BASE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
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
    let code = if status == 401 {
        "token_expired"
    } else if status == 400 {
        "bad_args"
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
    if let Some(title) = item
        .pointer("/properties/Name/title")
        .or_else(|| item.get("title"))
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_plain_text_reads_paragraph() {
        let block = json!({
            "id": "b1",
            "type": "paragraph",
            "has_children": false,
            "paragraph": {
                "rich_text": [{ "plain_text": "Hello from Notion" }]
            }
        });
        assert_eq!(block_plain_text(&block), "Hello from Notion");
        assert_eq!(map_block(&block)["text"], "Hello from Notion");
    }

    #[test]
    fn write_text_from_args_reads_text() {
        let args = json!({ "text": "hello" });
        assert_eq!(write_text_from_args(&args).as_deref(), Some("hello"));
    }

    #[test]
    fn write_text_from_args_joins_children_strings() {
        let args = json!({ "children": ["a", "b"] });
        assert_eq!(write_text_from_args(&args).as_deref(), Some("a\n\nb"));
    }

    #[test]
    fn write_text_from_args_null_children_is_none() {
        let args = json!({ "children": Value::Null });
        assert!(write_text_from_args(&args).is_none());
    }

    #[test]
    fn write_text_from_args_accepts_content_body_message_summary() {
        assert_eq!(
            write_text_from_args(&json!({ "content": "hello" })).as_deref(),
            Some("hello")
        );
        assert_eq!(
            write_text_from_args(&json!({ "body": "hello" })).as_deref(),
            Some("hello")
        );
        assert_eq!(
            write_text_from_args(&json!({ "message": "hello" })).as_deref(),
            Some("hello")
        );
        assert_eq!(
            write_text_from_args(&json!({ "summary": "hello" })).as_deref(),
            Some("hello")
        );
        assert!(write_text_from_args(&json!({ "content": "   " })).is_none());
        assert!(write_text_from_args(&json!({ "body": "   " })).is_none());
        assert!(write_text_from_args(&json!({ "message": "   " })).is_none());
        assert!(write_text_from_args(&json!({ "summary": "   " })).is_none());
        assert!(write_text_from_args(&json!({})).is_none());
    }

    #[test]
    fn write_text_from_args_children_string_or_object_items() {
        let as_string = json!({ "children": "hello" });
        assert_eq!(write_text_from_args(&as_string).as_deref(), Some("hello"));
        let objects = json!({ "children": [{ "text": "a" }, { "content": "b" }] });
        assert_eq!(write_text_from_args(&objects).as_deref(), Some("a\n\nb"));
        let block = json!({
            "children": [{
                "type": "paragraph",
                "paragraph": {
                    "rich_text": [{ "plain_text": "from a block" }]
                }
            }]
        });
        assert_eq!(
            write_text_from_args(&block).as_deref(),
            Some("from a block")
        );
    }

    #[test]
    fn write_text_from_args_named_key_wins_over_children() {
        let args = json!({ "text": "hello", "children": ["ignored"] });
        assert_eq!(write_text_from_args(&args).as_deref(), Some("hello"));
    }
}

pub(crate) fn block_plain_text(block: &Value) -> String {
    let ty = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let rich = block
        .get(ty)
        .and_then(|v| v.get("rich_text").or_else(|| v.get("text")))
        .and_then(|v| v.as_array());
    let Some(arr) = rich else {
        if ty == "child_page" || ty == "child_database" {
            return block
                .pointer(&format!("/{ty}/title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        return String::new();
    };
    arr.iter()
        .filter_map(|t| {
            t.get("plain_text")
                .or_else(|| t.pointer("/text/content"))
                .and_then(|v| v.as_str())
        })
        .collect()
}

pub(crate) fn map_block(block: &Value) -> Value {
    json!({
        "id": block.get("id"),
        "type": block.get("type"),
        "has_children": block.get("has_children"),
        "text": block_plain_text(block),
    })
}

/// Pull page body from the shapes models actually send (`text`, `content`,
/// `body`, `message`, `children` as a string or list of strings/blocks).
pub(crate) fn write_text_from_args(args: &Value) -> Option<String> {
    for key in ["text", "content", "body", "message", "summary"] {
        if let Some(s) = args
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    let children = args.get("children")?;
    if let Some(s) = children.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let arr = children.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(s.to_string());
            continue;
        }
        if let Some(s) = item
            .get("text")
            .or_else(|| item.get("content"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(s.to_string());
            continue;
        }
        let t = block_plain_text(item);
        if !t.is_empty() {
            parts.push(t);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

pub(crate) fn children_payload(text: &str) -> Value {
    let chunks: Vec<Value> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(50)
        .map(rich_text_paragraph)
        .collect();
    if chunks.is_empty() {
        json!([rich_text_paragraph(text)])
    } else {
        Value::Array(chunks)
    }
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
