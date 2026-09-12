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

const BACKOFF_STEP_MS: u64 = 400;
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Linear `400ms * attempt`, min 400ms. Cap so huge counts never overflow Duration.
pub fn retry_backoff(attempt: u32) -> Duration {
    let n = u64::from(attempt.max(1));
    Duration::from_millis(n.saturating_mul(BACKOFF_STEP_MS)).min(BACKOFF_CAP)
}

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
                let wait = retry_backoff(attempt);
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
    if matches!(
        code,
        "bad_args" | "not_connected" | "unknown_op" | "unknown_provider"
    ) {
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
    use fleet_auth::AppError;
    use serde_json::json;
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn provider_retries_network_and_429() {
        let cases = [
            ("cannot reach", true),
            ("timeout", true),
            ("timed out", true),
            ("rate limit", true),
            ("429", true),
            ("502", true),
            ("503", true),
            ("504", true),
            ("connection", true),
            ("temporar", true),
            ("try again", true),
            ("Grok rate limited — try again shortly", true),
            (
                "cannot reach Ollama at http://127.0.0.1:11434: connection refused. Start it with `ollama serve`.",
                true,
            ),
            (
                "model `llama` not found in Ollama. Pull it with: ollama pull llama",
                false,
            ),
            (
                "Grok auth failed — run `grok login` (or check XAI_API_KEY fallback)",
                false,
            ),
        ];
        for (msg, want) in cases {
            assert_eq!(
                is_retryable_provider(msg),
                want,
                "is_retryable_provider({msg:?}) == {want}"
            );
        }
    }

    #[test]
    fn is_retryable_provider_openai_claude_429_and_502() {
        assert!(is_retryable_provider(
            "OpenAI error (429 Too Many Requests): insufficient_quota"
        ));
        assert!(is_retryable_provider(
            "Claude error (502 Bad Gateway): overloaded_error"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_provider_retries_transient_then_ok() {
        tokio::time::pause();
        let calls = Cell::new(0u32);
        let result = retry_provider("test", || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                if n < 2 {
                    Err(AppError::bad_request("cannot reach provider"))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        result.expect("two cannot-reach failures then Ok must succeed");
        assert_eq!(
            calls.get(),
            3,
            "PROVIDER_ATTEMPTS=3: fail, fail, Ok must call the closure 3 times"
        );
    }

    #[tokio::test]
    async fn retry_provider_does_not_retry_auth_or_model_not_found() {
        for msg in [
            "Grok auth failed — run `grok login` (or check XAI_API_KEY fallback)",
            "model `llama` not found in Ollama. Pull it with: ollama pull llama",
        ] {
            let calls = Cell::new(0u32);
            let result: AppResult<()> = retry_provider("test", || {
                calls.set(calls.get() + 1);
                let msg = msg.to_string();
                async move { Err(AppError::bad_request(msg)) }
            })
            .await;
            let err = result.expect_err(msg);
            assert_eq!(err.to_string(), msg);
            assert_eq!(calls.get(), 1, "{msg} must return on the first attempt");
        }
    }

    fn chat_tool(ok: bool, summary: &str) -> ChatAction {
        ChatAction {
            tool: "integration_query".into(),
            ok,
            summary: summary.into(),
            run_id: None,
            sandbox_ids: None,
            detail: None,
        }
    }

    #[test]
    fn tool_retries_upstream_not_bad_args() {
        // Summary has no 503/timeout/connection tokens: only provider_status=503 retries.
        let fail = chat_tool(false, "Slack API unavailable");
        assert!(
            is_retryable_tool(
                &fail,
                &json!({ "error_code": "upstream", "provider_status": 503 })
            ),
            "provider_status=503 must retry when the summary has no 5xx/timeout tokens"
        );
    }

    #[test]
    fn tool_does_not_retry_bad_args() {
        let bad = chat_tool(false, "text is required");
        assert!(
            !is_retryable_tool(&bad, &json!({ "error_code": "bad_args" })),
            "bad_args must not replay the same call"
        );
    }

    #[test]
    fn tool_does_not_retry_400() {
        // Summary has no validation/undefined tokens: only the 4xx arm denies.
        let four = chat_tool(false, "Notion API rejected the page body");
        assert!(
            !is_retryable_tool(
                &four,
                &json!({ "error_code": "upstream", "provider_status": 400 })
            ),
            "400 must not replay the same payload"
        );
    }

    #[test]
    fn tool_ok_true_never_retries() {
        let ok = chat_tool(true, "Slack API unavailable");
        assert!(
            !is_retryable_tool(
                &ok,
                &json!({ "error_code": "upstream", "provider_status": 503 })
            ),
            "ok=true must not retry even with upstream 503"
        );
    }

    #[test]
    fn tool_retries_429_and_timeout_code() {
        let rate = chat_tool(false, "Slack API too many requests");
        assert!(
            is_retryable_tool(
                &rate,
                &json!({ "error_code": "upstream", "provider_status": 429 })
            ),
            "provider_status=429 is the 4xx exception and must retry"
        );
        let timed = chat_tool(false, "Slack API did not respond");
        assert!(
            is_retryable_tool(&timed, &json!({ "error_code": "timeout" })),
            "error_code=timeout with no status must retry"
        );
    }

    #[test]
    fn tool_does_not_retry_not_connected_or_unknown_op() {
        for code in ["not_connected", "unknown_op", "unknown_provider"] {
            let fail = chat_tool(false, "integration is missing");
            assert!(
                !is_retryable_tool(&fail, &json!({ "error_code": code })),
                "{code} must not retry a missing or unknown integration"
            );
        }
    }

    #[test]
    fn retry_backoff_first_attempts_are_400ms_steps() {
        assert_eq!(retry_backoff(1), Duration::from_millis(400));
        assert_eq!(retry_backoff(2), Duration::from_millis(800));
    }
}
