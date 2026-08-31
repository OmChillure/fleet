//! Discord user OAuth helpers (control plane only).

use serde_json::{Value, json};

use crate::proxy::types::ProxyResult;

pub(crate) async fn discord_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
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
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "discord",
        op,
        code,
        format!("Discord API {status}: {msg}"),
        Some(status),
    )
}
