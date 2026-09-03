//! Figma REST helpers (control plane only). `file_read` scope.

use serde_json::{Value, json};

use crate::proxy::types::ProxyResult;

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} is required"))
}

pub(crate) async fn figma_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match http
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
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
        .get("err")
        .or_else(|| body.get("message"))
        .or_else(|| body.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "figma",
        op,
        code,
        format!("Figma API {status}: {msg}"),
        Some(status),
    )
}

pub(crate) fn page_summaries(document: &Value) -> Vec<Value> {
    document
        .get("children")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|n| {
                    json!({
                        "id": n.get("id"),
                        "name": n.get("name"),
                        "type": n.get("type"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
