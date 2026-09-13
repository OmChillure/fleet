//! Notion search / page / database tools.

use serde_json::{json, Value};

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
    let origin = notion_api_origin();
    let (status, body) =
        notion_post(http, token, &format!("{origin}/v1/search"), &payload).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::dispatch;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    const LEAK: &str = "ntn_SEARCH_MUST_NOT_ECHO_c0ffee";

    struct OriginGuard;
    impl Drop for OriginGuard {
        fn drop(&mut self) {
            TEST_NOTION_API_BASE.with(|c| *c.borrow_mut() = None);
        }
    }

    struct NotionSink {
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        origin: String,
        last_json: Arc<Mutex<Option<Value>>>,
    }

    impl Drop for NotionSink {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    impl NotionSink {
        fn bind(status: u16, body: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind notion sink");
            listener.set_nonblocking(true).expect("nonblocking sink");
            let addr = listener.local_addr().expect("sink addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let last_json = Arc::new(Mutex::new(None));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
            let last_json_t = last_json.clone();
            let payload = body.to_string();
            thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            hits_t.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 4096];
                            loop {
                                match stream.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&tmp[..n]);
                                        if let Some(end) = headers_end(&buf) {
                                            let len = content_length(&buf[..end]).unwrap_or(0);
                                            if buf.len() >= end + len {
                                                break;
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            if let Some(end) = headers_end(&buf) {
                                if let Ok(v) = serde_json::from_slice::<Value>(&buf[end..]) {
                                    *last_json_t.lock().expect("last_json") = Some(v);
                                }
                            }
                            let resp = format!(
                                "HTTP/1.1 {status} ok\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                hits,
                stop,
                origin: format!("http://{addr}"),
                last_json,
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }

        fn last_json(&self) -> Option<Value> {
            self.last_json.lock().expect("last_json").clone()
        }
    }

    fn headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        let text = std::str::from_utf8(headers).ok()?;
        for line in text.split("\r\n") {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
        None
    }

    fn mapped_page() -> Value {
        json!({
            "object": "page",
            "id": "page-roadmap",
            "url": "https://www.notion.so/page-roadmap",
            "last_edited_time": "2024-01-01T00:00:00.000Z",
            "parent": { "type": "workspace", "workspace": true },
            "properties": {
                "title": {
                    "title": [{ "plain_text": "Roadmap" }]
                }
            }
        })
    }

    async fn dispatch_search(origin: &str, args: Value) -> ProxyResult {
        let _guard = OriginGuard;
        TEST_NOTION_API_BASE.with(|c| *c.borrow_mut() = Some(origin.to_string()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        dispatch("notion", &http, LEAK, "search", &args, &json!({})).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notion_search_maps_results_and_omits_blank_query() {
        let sink = NotionSink::bind(
            200,
            json!({
                "results": [mapped_page()],
                "access_token": LEAK
            }),
        );
        let result = dispatch_search(&sink.origin, json!({ "query": "" })).await;
        assert!(result.ok, "search 200 must be ok: {result:?}");
        assert_eq!(result.provider, "notion", "{result:?}");
        assert_eq!(result.operation, "search", "{result:?}");
        assert_eq!(result.data["count"], json!(1), "count must match items: {result:?}");
        assert_eq!(result.data["query"], json!(""), "{result:?}");
        let item = &result.data["items"][0];
        assert_eq!(item["id"], json!("page-roadmap"), "must map results[].id: {result:?}");
        assert_eq!(item["object"], json!("page"), "must map results[].object: {result:?}");
        assert_eq!(
            item["url"],
            json!("https://www.notion.so/page-roadmap"),
            "must map results[].url: {result:?}"
        );
        assert_eq!(item["title"], json!("Roadmap"), "must map title: {result:?}");
        let posted = sink.last_json().unwrap_or_else(|| panic!("POST body: {result:?}"));
        assert_eq!(posted, json!({ "page_size": 20 }), "blank query must POST page_size only: {posted}");
        assert!(
            posted.get("query").is_none(),
            "blank query must omit query key: {posted}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK) && !dump.to_ascii_lowercase().contains("bearer"),
            "search must not echo bearer/token: {dump}"
        );
        assert!(sink.hits() > 0, "search must hit the sink: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notion_search_nonempty_query_is_posted_and_echoed() {
        let sink = NotionSink::bind(200, json!({ "results": [mapped_page()] }));
        let result = dispatch_search(&sink.origin, json!({ "query": "roadmap" })).await;
        assert!(result.ok, "search 200 must be ok: {result:?}");
        assert_eq!(result.data["query"], json!("roadmap"), "data.query must echo: {result:?}");
        let posted = sink.last_json().unwrap_or_else(|| panic!("POST body: {result:?}"));
        assert_eq!(
            posted.get("query"),
            Some(&json!("roadmap")),
            "payload.query must be posted: {posted}"
        );
        assert_eq!(posted["page_size"], json!(20), "{posted}");
        assert!(sink.hits() > 0, "search must hit the sink: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notion_search_401_is_token_expired() {
        let sink = NotionSink::bind(
            401,
            json!({ "message": "API token is invalid.", "access_token": LEAK }),
        );
        let result = dispatch_search(&sink.origin, json!({ "query": "roadmap" })).await;
        assert!(!result.ok, "401 must be ok:false: {result:?}");
        assert_eq!(
            result.error_code.as_deref(),
            Some("token_expired"),
            "Notion 401 must map to token_expired: {result:?}"
        );
        assert_eq!(result.provider, "notion", "{result:?}");
        assert_eq!(result.operation, "search", "{result:?}");
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(!dump.contains(LEAK), "token_expired must not echo token: {dump}");
        assert!(sink.hits() > 0, "401 must hit the sink: {result:?}");
    }
}
