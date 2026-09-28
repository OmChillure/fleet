//! Confluence Cloud REST helpers (same Atlassian cloud_id as Jira).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

pub(crate) fn base(cloud_id: &str) -> String {
    format!("https://api.atlassian.com/ex/confluence/{cloud_id}/wiki/rest/api")
}

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} is required"))
}

pub(crate) fn map_page(p: &Value) -> Value {
    let body = p
        .get("body")
        .and_then(|b| b.get("storage").or_else(|| b.get("view")))
        .and_then(|s| s.get("value"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "id": p.get("id"),
        "title": p.get("title"),
        "type": p.get("type"),
        "status": p.get("status"),
        "space": p.get("space").and_then(|s| s.get("key")),
        "version": p.get("version").and_then(|v| v.get("number")),
        "body": body,
        "links": p.get("_links").and_then(|l| l.get("webui")),
    })
}

pub(crate) fn map_space(s: &Value) -> Value {
    json!({
        "key": s.get("key"),
        "name": s.get("name"),
        "type": s.get("type"),
        "status": s.get("status"),
    })
}

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
}

pub(crate) async fn cf_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn cf_post(
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

pub(crate) async fn cf_put(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match headers(http.put(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn results(body: &Value) -> Vec<Value> {
    body.get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "confluence",
        op,
        code,
        format!("{msg} ({status})"),
        Some(status),
    )
}
