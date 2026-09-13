//! Confluence space / page tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn search(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let q = args
        .get("query")
        .or_else(|| args.get("cql"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("type=page");
    let cql = if q.contains('=') || q.contains('~') {
        q.to_string()
    } else {
        format!("type=page AND text ~ \"{q}\"")
    };
    let enc = urlencoding::optional(&cql);
    let url = format!(
        "{}/content/search?cql={enc}&limit=20&expand=space,version",
        base(cloud_id)
    );
    let (status, body) = cf_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("search", status, &body);
    }
    let items: Vec<Value> = results(&body).iter().map(map_page).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "confluence",
        "search",
        format!("Found {n} Confluence pages"),
        json!({ "count": n, "query": q, "items": items }),
        Some(status),
    )
}

mod urlencoding {
    pub fn optional(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
}

pub(crate) async fn list_spaces(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    _args: &Value,
) -> ProxyResult {
    let url = format!("{}/space?limit=50", base(cloud_id));
    let (status, body) = cf_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_spaces", status, &body);
    }
    let items: Vec<Value> = results(&body).iter().map(map_space).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "confluence",
        "list_spaces",
        format!("Listed {n} Confluence spaces"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_pages(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let space = args
        .get("space")
        .or_else(|| args.get("spaceKey"))
        .and_then(|v| v.as_str());
    let mut url = format!("{}/content?type=page&limit=25", base(cloud_id));
    if let Some(s) = space {
        url.push_str("&spaceKey=");
        url.push_str(s);
    }
    let (status, body) = cf_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_pages", status, &body);
    }
    let items: Vec<Value> = results(&body).iter().map(map_page).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "confluence",
        "list_pages",
        format!("Listed {n} Confluence pages"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_page(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "page_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("confluence", "get_page", "bad_args", e, None),
    };
    let url = format!(
        "{}/content/{id}?expand=body.storage,space,version",
        base(cloud_id)
    );
    let (status, body) = cf_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_page", status, &body);
    }
    ProxyResult::ok_result(
        "confluence",
        "get_page",
        "Fetched Confluence page",
        map_page(&body),
        Some(status),
    )
}

pub(crate) async fn create_page(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let space = match req_str(args, "space").or_else(|_| req_str(args, "spaceKey")) {
        Ok(s) => s,
        Err(e) => return ProxyResult::err("confluence", "create_page", "bad_args", e, None),
    };
    let title = match req_str(args, "title") {
        Ok(t) => t,
        Err(e) => return ProxyResult::err("confluence", "create_page", "bad_args", e, None),
    };
    let text = args
        .get("text")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let html = if text.contains('<') {
        text.to_string()
    } else {
        format!("<p>{}</p>", html_escape(text))
    };
    let url = format!("{}/content", base(cloud_id));
    let payload = json!({
        "type": "page",
        "title": title,
        "space": { "key": space },
        "body": { "storage": { "value": html, "representation": "storage" } }
    });
    let (status, body) = cf_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_page", status, &body);
    }
    ProxyResult::ok_result(
        "confluence",
        "create_page",
        format!("Created Confluence page `{title}`"),
        map_page(&body),
        Some(status),
    )
}

pub(crate) async fn update_page(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "page_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("confluence", "update_page", "bad_args", e, None),
    };
    let get_url = format!(
        "{}/content/{id}?expand=version,space,body.storage",
        base(cloud_id)
    );
    let (gstatus, existing) = cf_get(http, token, &get_url).await;
    if !(200..300).contains(&gstatus) {
        return upstream_err("update_page", gstatus, &existing);
    }
    let version = existing
        .pointer("/version/number")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        + 1;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            existing
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("Untitled")
        });
    let text = args
        .get("text")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prev = existing
        .pointer("/body/storage/value")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let html = if args
        .get("replace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if text.contains('<') {
            text.to_string()
        } else {
            format!("<p>{}</p>", html_escape(text))
        }
    } else if text.is_empty() {
        prev.to_string()
    } else {
        format!("{prev}<p>{}</p>", html_escape(text))
    };
    let url = format!("{}/content/{id}", base(cloud_id));
    let payload = json!({
        "id": id,
        "type": "page",
        "title": title,
        "version": { "number": version },
        "body": { "storage": { "value": html, "representation": "storage" } }
    });
    let (status, body) = cf_put(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("update_page", status, &body);
    }
    ProxyResult::ok_result(
        "confluence",
        "update_page",
        "Updated Confluence page",
        map_page(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "page_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("confluence", "add_comment", "bad_args", e, None),
    };
    let text = match req_str(args, "text").or_else(|_| req_str(args, "body")) {
        Ok(t) => t,
        Err(e) => return ProxyResult::err("confluence", "add_comment", "bad_args", e, None),
    };
    let url = format!("{}/content/{id}/child/comment", base(cloud_id));
    let payload = json!({
        "type": "comment",
        "container": { "id": id, "type": "page" },
        "body": { "storage": { "value": format!("<p>{}</p>", html_escape(&text)), "representation": "storage" } }
    });
    let (status, body) = cf_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "confluence",
        "add_comment",
        "Added Confluence comment",
        json!({ "id": body.get("id"), "ok": true }),
        Some(status),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
