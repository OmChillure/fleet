//! Chat turn on a paired runner via unmodified official CLI.
//!
//! The control plane never reads the runner's `~/.grok/auth.json` (or Claude
//! login). It enqueues `chat_turn`; fleet-host runs `grok` / `claude` / `ollama`.

use std::time::Duration;

use chrono::{TimeDelta, Utc};
use fleet_auth::{AppError, AppResult};
use fleet_db::repos::work_claims;
use fleet_db::{NewWorkClaim, WorkerHost};
use serde_json::{Value, json};
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
    let host = crate::worker_hosts::load_visible_host(state, workspace_id, runner_id).await?;
    ensure_online(&host)?;

    let prompt = format_transcript(pairs);
    if prompt.trim().is_empty() {
        return Err(AppError::bad_request("empty chat turn".to_string()));
    }
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

fn ensure_online(host: &WorkerHost) -> AppResult<()> {
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

    #[test]
    fn transcript_labels_roles() {
        let t = format_transcript(&[
            ("user".into(), "hi".into()),
            ("assistant".into(), "hello".into()),
        ]);
        assert!(t.contains("User:\nhi"));
        assert!(t.contains("Assistant:\nhello"));
    }
}
