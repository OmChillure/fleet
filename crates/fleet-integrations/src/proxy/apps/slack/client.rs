//! Slack Web API helpers (control plane only).

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

pub(crate) fn slack_api_origin() -> String {
    #[cfg(test)]
    if let Some(base) = TEST_SLACK_API_BASE.with(|c| c.borrow().clone()) {
        return base;
    }
    "https://slack.com/api".into()
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_SLACK_API_BASE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) async fn slack_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
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

pub(crate) async fn slack_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match http
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json; charset=utf-8")
        .json(body)
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

/// Slack often returns HTTP 200 with `{ "ok": false, "error": "..." }`.
pub(crate) fn slack_ok(status: u16, body: &Value, op: &str) -> Result<(), ProxyResult> {
    if !(200..300).contains(&status) {
        return Err(upstream_err(op, status, body));
    }
    if body.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_error");
        let code = if err == "invalid_auth" || err == "token_revoked" || err == "not_authed" {
            "token_expired"
        } else {
            "upstream"
        };
        return Err(ProxyResult::err(
            "slack",
            op,
            code,
            format!("Slack API: {err}"),
            Some(status),
        ));
    }
    Ok(())
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "slack",
        op,
        code,
        format!("Slack API {status}: {msg}"),
        Some(status),
    )
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
