//! Provider-agnostic retries (Grok, Ollama, later Claude).
//! Same idea as Claude Code / Grok: transient fail → retry the call;
//! tool fail → stay in the loop and retry the user task.

use std::time::Duration;

use fleet_auth::AppResult;
use serde_json::Value;

use crate::chat_tools::ChatAction;

pub const PROVIDER_ATTEMPTS: u32 = 3;
pub const TOOL_TRANSIENT_RETRIES: u32 = 2;
pub const EXTRA_ROUNDS_ON_FAIL: usize = 3;

pub async fn retry_provider<T, F, Fut>(label: &str, mut f: F) -> AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if is_retryable_provider(&e.to_string()) && attempt + 1 < PROVIDER_ATTEMPTS => {
                attempt += 1;
                let wait = Duration::from_millis(400 * u64::from(attempt));
                tracing::warn!(
                    attempt,
                    error = %e,
                    wait_ms = wait.as_millis() as u64,
                    "{label}: retrying provider call"
                );
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn is_retryable_provider(msg: &str) -> bool {
    let s = msg.to_ascii_lowercase();
    s.contains("cannot reach")
        || s.contains("timeout")
        || s.contains("timed out")
        || s.contains("rate limit")
        || s.contains("429")
        || s.contains("502")
        || s.contains("503")
        || s.contains("504")
        || s.contains("connection")
        || s.contains("temporar")
        || s.contains("try again")
}

pub fn is_retryable_tool(action: &ChatAction, content: &Value) -> bool {
    if action.ok {
        return false;
    }
    let code = content
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if matches!(code, "bad_args" | "not_connected" | "unknown_op" | "unknown_provider") {
        return false;
    }
    let status = content
        .get("provider_status")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // 4xx (except 429) is the model's args / permissions — do not replay the same call.
    if status > 0 && status < 500 && status != 429 {
        return false;
    }
    let sum = action.summary.to_ascii_lowercase();
    if sum.contains("validation") || sum.contains("should be defined") || sum.contains("undefined")
    {
        return false;
    }
    code == "timeout"
        || status == 0
        || status == 429
        || (500..600).contains(&status)
        || is_retryable_provider(&sum)
}

pub fn fail_nudge(failed: &[&ChatAction]) -> String {
    let bits: Vec<String> = failed
        .iter()
        .map(|a| format!("`{}` failed: {}", a.tool, a.summary))
        .collect();
    format!(
        "[retry] {}. Retry the original user task now — call the tool again \
         (same or fixed args). Do not stop or only apologize.",
        bits.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_retries_network_and_429() {
        assert!(is_retryable_provider("cannot reach Ollama at http://x: timeout"));
        assert!(is_retryable_provider("Grok rate limited — try again shortly"));
        assert!(is_retryable_provider("Grok error (503): unavailable"));
        assert!(!is_retryable_provider("model `llama` not found in Ollama"));
        assert!(!is_retryable_provider("Grok auth failed — run grok login"));
    }

    #[test]
    fn tool_retries_upstream_not_bad_args() {
        let fail = ChatAction {
            tool: "integration_query".into(),
            ok: false,
            summary: "Slack API 503: unavailable".into(),
            run_id: None,
            sandbox_ids: None,
            detail: None,
        };
        assert!(is_retryable_tool(
            &fail,
            &json!({ "error_code": "upstream", "provider_status": 503 })
        ));
        let bad = ChatAction {
            tool: "integration_query".into(),
            ok: false,
            summary: "text is required".into(),
            run_id: None,
            sandbox_ids: None,
            detail: None,
        };
        assert!(!is_retryable_tool(
            &bad,
            &json!({ "error_code": "bad_args" })
        ));
        let four = ChatAction {
            tool: "integration_query".into(),
            ok: false,
            summary: "Notion API 400: body.children should be defined, instead was `undefined`."
                .into(),
            run_id: None,
            sandbox_ids: None,
            detail: None,
        };
        assert!(
            !is_retryable_tool(
                &four,
                &json!({ "error_code": "upstream", "provider_status": 400 })
            ),
            "400 must not replay the same payload"
        );
    }
}
