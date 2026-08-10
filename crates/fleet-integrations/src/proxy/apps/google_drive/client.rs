//! Google Drive v3 helpers (control plane only). Readonly scope.

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

pub(crate) async fn drive_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match http
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let ct = res
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if ct.contains("application/json") || ct.is_empty() {
                let body = res.json::<Value>().await.unwrap_or(json!({}));
                (status, body)
            } else {
                let text = res.text().await.unwrap_or_default();
                (status, json!({ "text": truncate_str(&text, 8000) }))
            }
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
        "google_drive",
        op,
        code,
        format!("Google Drive API {status}: {msg}"),
        Some(status),
    )
}

pub(crate) fn map_file(f: &Value) -> Value {
    json!({
        "id": f.get("id"),
        "name": f.get("name"),
        "mimeType": f.get("mimeType"),
        "modifiedTime": f.get("modifiedTime"),
        "webViewLink": f.get("webViewLink"),
        "owners": f.get("owners").and_then(|o| o.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|u| u.get("emailAddress").or_else(|| u.get("displayName")))
                .cloned()
                .collect::<Vec<_>>()
        }),
        "size": f.get("size"),
    })
}
