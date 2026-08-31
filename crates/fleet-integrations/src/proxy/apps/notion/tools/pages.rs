//! Notion search / page / database tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn search(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let page_size = args
        .get("page_size")
        .or_else(|| args.get("limit"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let mut payload = json!({ "page_size": page_size });
    if !query.is_empty() {
        payload["query"] = json!(query);
    }
    if let Some(filter) = args.get("filter") {
        payload["filter"] = filter.clone();
    }
    let (status, body) =
        notion_post(http, token, "https://api.notion.com/v1/search", &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("search", status, &body);
    }
    let items: Vec<Value> = body
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_search_item).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "notion",
        "search",
        format!("Found {n} Notion results"),
        json!({ "count": n, "query": query, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_page(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let id = match req_str(args, "page_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("notion", "get_page", "bad_args", e, None),
    };
    let url = format!("https://api.notion.com/v1/pages/{id}");
    let (status, body) = notion_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_page", status, &body);
    }
    ProxyResult::ok_result(
        "notion",
        "get_page",
        "Fetched Notion page",
        map_search_item(&body),
        Some(status),
    )
}

pub(crate) async fn get_database(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let id = match req_str(args, "database_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("notion", "get_database", "bad_args", e, None),
    };
    let url = format!("https://api.notion.com/v1/databases/{id}");
    let (status, body) = notion_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_database", status, &body);
    }
    let props = body
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| json!({ "name": k, "type": v.get("type") }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ProxyResult::ok_result(
        "notion",
        "get_database",
        "Fetched Notion database",
        json!({
            "id": body.get("id"),
            "title": map_search_item(&body).get("title"),
            "url": body.get("url"),
            "properties": props,
        }),
        Some(status),
    )
}

pub(crate) async fn query_database(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "database_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("notion", "query_database", "bad_args", e, None),
    };
    let page_size = args
        .get("page_size")
        .or_else(|| args.get("limit"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let mut payload = json!({ "page_size": page_size });
    if let Some(filter) = args.get("filter") {
        payload["filter"] = filter.clone();
    }
    if let Some(sorts) = args.get("sorts") {
        payload["sorts"] = sorts.clone();
    }
    let url = format!("https://api.notion.com/v1/databases/{id}/query");
    let (status, body) = notion_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("query_database", status, &body);
    }
    let items: Vec<Value> = body
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_search_item).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "notion",
        "query_database",
        format!("Queried {n} Notion database rows"),
        json!({ "database_id": id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_block_children(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "block_id")
        .or_else(|_| req_str(args, "page_id"))
        .or_else(|_| req_str(args, "id"))
    {
        Ok(i) => i,
        Err(e) => {
            return ProxyResult::err("notion", "get_block_children", "bad_args", e, None);
        }
    };
    let page_size = args
        .get("page_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 50);
    let url = format!("https://api.notion.com/v1/blocks/{id}/children?page_size={page_size}");
    let (status, body) = notion_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_block_children", status, &body);
    }
    let items: Vec<Value> = body
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_block).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "notion",
        "get_block_children",
        format!("Listed {n} Notion blocks"),
        json!({ "block_id": id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn create_page(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled");
    let parent_page = args
        .get("parent_page_id")
        .or_else(|| args.get("page_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let parent_db = args
        .get("parent_database_id")
        .or_else(|| args.get("database_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let parent = match (parent_page, parent_db) {
        (Some(id), _) => json!({ "page_id": id }),
        (_, Some(id)) => json!({ "database_id": id }),
        _ => {
            return ProxyResult::err(
                "notion",
                "create_page",
                "bad_args",
                "parent_page_id or parent_database_id is required",
                None,
            );
        }
    };
    let mut payload = json!({
        "parent": parent,
        "properties": {
            "title": {
                "title": [{ "type": "text", "text": { "content": title } }]
            }
        }
    });
    if parent_db.is_some() {
        // Databases often use "Name" not "title".
        payload["properties"] = json!({
            "Name": {
                "title": [{ "type": "text", "text": { "content": title } }]
            }
        });
    }
    let Some(text) = write_text_from_args(args) else {
        return ProxyResult::err(
            "notion",
            "create_page",
            "bad_args",
            "text is required — pass args.text with the page body (Slack request, PR link, what shipped). A title-only page is empty and not a result.",
            None,
        );
    };
    payload["children"] = children_payload(&text);
    let (status, body) =
        notion_post(http, token, "https://api.notion.com/v1/pages", &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_page", status, &body);
    }
    ProxyResult::ok_result(
        "notion",
        "create_page",
        format!("Created Notion page `{title}`"),
        json!({
            "id": body.get("id"),
            "url": body.get("url"),
            "title": title,
        }),
        Some(status),
    )
}

pub(crate) async fn append_blocks(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "block_id")
        .or_else(|_| req_str(args, "page_id"))
        .or_else(|_| req_str(args, "id"))
    {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("notion", "append_blocks", "bad_args", e, None),
    };
    let Some(text) = write_text_from_args(args) else {
        return ProxyResult::err(
            "notion",
            "append_blocks",
            "bad_args",
            "text is required — pass args.text (the Slack messages). Do not send children: null.",
            None,
        );
    };
    let payload = json!({ "children": children_payload(&text) });
    let url = format!("https://api.notion.com/v1/blocks/{id}/children");
    let (status, body) = notion_patch(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("append_blocks", status, &body);
    }
    let n = body
        .get("results")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    ProxyResult::ok_result(
        "notion",
        "append_blocks",
        format!("Appended {n} Notion block(s)"),
        json!({ "block_id": id, "appended": n }),
        Some(status),
    )
}
