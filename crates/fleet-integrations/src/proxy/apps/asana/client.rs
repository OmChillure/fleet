//! Asana HTTP helpers (REST 1.0).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

const API: &str = "https://app.asana.com/api/1.0";

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

pub(crate) fn map_workspace(w: &Value) -> Value {
    json!({
        "gid": w.get("gid"),
        "name": w.get("name"),
        "resource_type": w.get("resource_type"),
    })
}

pub(crate) fn map_project(p: &Value) -> Value {
    json!({
        "gid": p.get("gid"),
        "name": p.get("name"),
        "archived": p.get("archived"),
        "workspace": p.get("workspace").and_then(|w| w.get("gid").or_else(|| w.get("name"))),
        "permalink_url": p.get("permalink_url"),
    })
}

pub(crate) fn map_task(t: &Value) -> Value {
    json!({
        "gid": t.get("gid"),
        "name": t.get("name"),
        "notes": truncate_str(t.get("notes").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "completed": t.get("completed"),
        "assignee": t.get("assignee").and_then(|a| a.get("name").or_else(|| a.get("gid"))),
        "projects": t.get("projects"),
        "due_on": t.get("due_on"),
        "permalink_url": t.get("permalink_url"),
        "modified_at": t.get("modified_at"),
    })
}

pub(crate) fn unwrap_data(body: &Value) -> Value {
    body.get("data").cloned().unwrap_or_else(|| body.clone())
}

fn asana_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
}

pub(crate) async fn asana_get(http: &reqwest::Client, token: &str, path: &str) -> (u16, Value) {
    match asana_headers(http.get(format!("{API}{path}")), token)
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

pub(crate) async fn asana_post(
    http: &reqwest::Client,
    token: &str,
    path: &str,
    data: &Value,
) -> (u16, Value) {
    match asana_headers(http.post(format!("{API}{path}")), token)
        .json(&json!({ "data": data }))
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

pub(crate) async fn asana_put(
    http: &reqwest::Client,
    token: &str,
    path: &str,
    data: &Value,
) -> (u16, Value) {
    match asana_headers(http.put(format!("{API}{path}")), token)
        .json(&json!({ "data": data }))
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

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .pointer("/errors/0/message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "asana",
        op,
        code,
        format!("Asana API {status}: {msg}"),
        Some(status),
    )
}
