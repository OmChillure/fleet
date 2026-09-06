//! Chat turn on a paired runner via unmodified official CLI.
//!
//! The control plane never reads the runner's `~/.grok/auth.json` (or Claude
//! login). It enqueues `chat_turn`; fleet-host runs `grok` / `claude` / `ollama`.

use std::time::Duration;

use chrono::{TimeDelta, Utc};
use fleet_auth::{AppError, AppResult};
use fleet_db::repos::work_claims;
use fleet_db::{NewWorkClaim, WorkerHost};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chat::ChatProgress;
use crate::state::AppState;

const POLL: Duration = Duration::from_millis(400);
const WAIT: Duration = Duration::from_secs(240);
const OFFLINE_AFTER: TimeDelta = TimeDelta::seconds(90);

pub fn runner_cli_provider(provider: &str) -> bool {
    matches!(provider, "grok" | "xai" | "claude" | "anthropic" | "ollama")
}

pub fn format_transcript(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (role, content) in pairs {
        let label = match role.as_str() {
            "assistant" => "Assistant",
            "system" => "System",
            _ => "User",
        };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(label);
        out.push_str(":\n");
        out.push_str(content.trim());
    }
    out
}

/// Empty or whitespace-only pairs — ignore `format_transcript` role labels.
fn chat_turn_is_empty(pairs: &[(String, String)]) -> bool {
    pairs.iter().all(|(_, content)| content.trim().is_empty())
}

pub async fn run_on_runner(
    state: &AppState,
    workspace_id: Uuid,
    runner_id: Uuid,
    provider: &str,
    model: &str,
    effort: Option<&str>,
    pairs: &[(String, String)],
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<(String, String)> {
    if chat_turn_is_empty(pairs) {
        return Err(AppError::bad_request("empty chat turn".to_string()));
    }
    let host = require_taking(state, workspace_id, runner_id).await?;

    let prompt = format_transcript(pairs);
    let claim_id = Uuid::now_v7();
    let payload = json!({
        "kind": "chat_turn",
        "host_id": host.id,
        "provider": provider,
        "model": model,
        "effort": effort,
        "prompt": prompt,
        "timeout_s": 180,
    });
    work_claims::enqueue(
        &state.db,
        &NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "chat_turn".into(),
            payload,
            max_attempts: 1,
            available_at: Utc::now(),
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    emit(
        progress,
        ChatProgress::Status {
            text: format!("Running {provider} on {}…", host.name),
        },
    )
    .await;

    wait_for_result(&state.db, claim_id, model, progress, &host.name, provider).await
}

/// `DELETE` revoke sets `status=disabled` and drops the machine token, but
/// leaves `enabled=true` and a fresh `last_seen_at`. Chat must 400 on that
/// leftover — not enqueue `chat_turn` and wait 240s.
pub(crate) fn host_is_removed(host: &WorkerHost) -> bool {
    host.status == fleet_db::WORKER_HOST_STATUS_DISABLED
}

/// Workspace-visible runner that is still taking chat. Call before enqueue
/// and before NDJSON so a leftover `runner_id` 400s instead of hanging.
pub(crate) async fn require_taking(
    state: &AppState,
    workspace_id: Uuid,
    runner_id: Uuid,
) -> AppResult<WorkerHost> {
    let host = crate::worker_hosts::load_visible_host(state, workspace_id, runner_id).await?;
    ensure_online(&host)?;
    Ok(host)
}

fn ensure_online(host: &WorkerHost) -> AppResult<()> {
    if host_is_removed(host) {
        return Err(AppError::bad_request(format!(
            "runner `{}` is removed",
            host.name
        )));
    }
    let Some(seen) = host.last_seen_at else {
        return Err(AppError::bad_request(format!(
            "runner `{}` has not heartbeated yet",
            host.name
        )));
    };
    if Utc::now() - seen > OFFLINE_AFTER {
        return Err(AppError::bad_request(format!(
            "runner `{}` is offline",
            host.name
        )));
    }
    if !host.enabled || host.paused || host.drain {
        return Err(AppError::bad_request(format!(
            "runner `{}` is not taking jobs",
            host.name
        )));
    }
    // Mid host-bin swap: /me/lease is claim:null; chat must 400 here instead
    // of enqueue + 240s while the box has stopped polling.
    if host.software_updating {
        return Err(AppError::bad_request(format!(
            "runner `{}` is updating / not taking jobs (host is updating)",
            host.name
        )));
    }
    // v1 is host mode only. allow_host=false or advertised_host=false already
    // makes /me/lease claim:null; chat must 400 here instead of enqueue + 240s.
    if !host.effective_host() {
        return Err(AppError::bad_request(format!(
            "runner `{}` is not taking / host jobs not allowed",
            host.name
        )));
    }
    if fleet_db::repos::worker_hosts::at_job_cap(host) {
        return Err(AppError::bad_request(format!(
            "runner `{}` is at cap (too many jobs / not taking more)",
            host.name
        )));
    }
    Ok(())
}

async fn wait_for_result(
    pool: &fleet_db::PgPool,
    claim_id: Uuid,
    fallback_model: &str,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    host_name: &str,
    provider: &str,
) -> AppResult<(String, String)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let started = tokio::time::Instant::now();
    let mut last_tick = 0u64;
    loop {
        let claim = work_claims::get(pool, claim_id)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .ok_or_else(|| AppError::internal("chat_turn claim vanished".to_string()))?;
        if claim.status == "done" {
            return parse_result(&claim.payload, fallback_model);
        }
        if claim.status == "dead" {
            return Err(AppError::bad_request(
                claim
                    .last_error
                    .unwrap_or_else(|| "runner chat turn failed".into()),
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::bad_request(
                "runner did not finish the chat turn in time — is fleet-host from this tree running?"
                    .to_string(),
            ));
        }
        let secs = started.elapsed().as_secs();
        if secs >= 5 && secs / 5 > last_tick {
            last_tick = secs / 5;
            emit(
                progress,
                ChatProgress::Status {
                    text: format!("Running {provider} on {host_name}… {secs}s"),
                },
            )
            .await;
        }
        tokio::time::sleep(POLL).await;
    }
}

fn parse_result(payload: &Value, fallback_model: &str) -> AppResult<(String, String)> {
    let result = payload.get("result").cloned().unwrap_or(Value::Null);
    if result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("official CLI on the runner failed");
        return Err(AppError::bad_request(err.to_string()));
    }
    let text = result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(AppError::bad_request(
            "runner CLI returned an empty reply".to_string(),
        ));
    }
    let model = result
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_model)
        .to_string();
    Ok((model, text))
}

async fn emit(progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>, ev: ChatProgress) {
    if let Some(tx) = progress {
        let _ = tx.send(ev).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[test]
    fn transcript_labels_roles() {
        let t = format_transcript(&[
            ("user".into(), "hi".into()),
            ("assistant".into(), "hello".into()),
        ]);
        assert_eq!(
            t,
            "User:\nhi\n\nAssistant:\nhello",
            "two-turn CLI prompt must keep User:/Assistant: order and a blank-line separator"
        );
    }

    #[test]
    fn format_transcript_trims_pair_content() {
        let t = format_transcript(&[("user".into(), "  hi\nthere  ".into())]);
        assert_eq!(
            t,
            "User:\nhi\nthere",
            "format_transcript must trim pair content (resumed DB history is not trimmed first)"
        );
    }

    /// Done claim whose CLI text is empty or whitespace must 400 (do not persist
    /// an empty assistant turn). Empty HTTP `messages` is locked in chat_stream_http.
    #[test]
    fn parse_result_empty_or_whitespace_cli_text_is_bad_request() {
        for (label, payload) in [
            ("whitespace", json!({ "result": { "ok": true, "text": "  " } })),
            ("empty", json!({ "result": { "ok": true, "text": "" } })),
            ("missing text", json!({ "result": { "ok": true } })),
        ] {
            let err = parse_result(&payload, "m").expect_err(label);
            assert!(
                matches!(
                    &err,
                    AppError::BadRequest(m) if m == "runner CLI returned an empty reply"
                ),
                "{label}: {err:?}"
            );
            assert_eq!(
                err.into_response().status(),
                StatusCode::BAD_REQUEST,
                "{label}"
            );
        }
    }

    #[test]
    fn parse_result_ok_false_surfaces_cli_error() {
        for (label, payload, want) in [
            (
                "error field",
                json!({ "result": { "ok": false, "error": "grok: model not found" } }),
                "grok: model not found",
            ),
            (
                "missing error",
                json!({ "result": { "ok": false } }),
                "official CLI on the runner failed",
            ),
        ] {
            let err = parse_result(&payload, "m").expect_err(label);
            assert!(
                matches!(&err, AppError::BadRequest(m) if m == want),
                "{label}: {err:?}"
            );
            assert_eq!(
                err.into_response().status(),
                StatusCode::BAD_REQUEST,
                "{label}"
            );
        }
    }

    #[test]
    fn parse_result_missing_model_uses_fallback() {
        for (label, payload) in [
            ("absent", json!({ "result": { "ok": true, "text": "hello" } })),
            (
                "empty",
                json!({ "result": { "ok": true, "text": "hello", "model": "" } }),
            ),
        ] {
            let (model, text) = parse_result(&payload, "requested-model").expect(label);
            assert_eq!(
                (model.as_str(), text.as_str()),
                ("requested-model", "hello"),
                "{label}"
            );
        }
    }

    /// DELETE leftover: status=disabled but enabled + last_seen still set.
    /// Dropping `ensure_online`'s first branch would then pass last_seen/enabled.
    fn leftover_disabled_host() -> WorkerHost {
        let now = Utc::now();
        WorkerHost {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            name: "revoked-leftover".into(),
            status: fleet_db::WORKER_HOST_STATUS_DISABLED.into(),
            enabled: true,
            paused: false,
            drain: false,
            isolation: "native".into(),
            allow_host: true,
            allow_container: false,
            max_concurrent: 2,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: None,
            advertised_host: true,
            advertised_container: false,
            platform: Some("linux-x86_64".into()),
            hostname: Some("revoked-leftover".into()),
            last_seen_at: Some(now),
            last_error: None,
            current_jobs: 0,
            advertised_max_concurrent: None,
            software_git_sha: None,
            software_bins: None,
            software_reported_at: Some(now),
            software_updating: false,
            auto_update: true,
            update_requested_at: None,
            machine_id: None,
            hardware: None,
            agent_runtimes: json!([]),
            settings_gen: 0,
            settings_ack_gen: 0,
            install_request: None,
            install_requested_at: None,
            runtime_settings: json!({}),
            runtime_reported: json!({}),
            created_by: None,
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn host_is_removed_true_when_status_disabled() {
        let host = leftover_disabled_host();
        assert!(
            host_is_removed(&host),
            "status=disabled leftover (enabled + last_seen still set) must be removed"
        );
        let err = ensure_online(&host).expect_err(
            "ensure_online must fail on DELETE leftover before last_seen/enabled",
        );
        assert!(
            matches!(&err, AppError::BadRequest(m) if m.contains("removed")),
            "first ensure_online branch is host_is_removed, not offline/not-taking: {err:?}"
        );
    }
}
