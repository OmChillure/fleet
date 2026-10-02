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
use crate::chat_tools::{
    ChatAction, ChatLlmContext, MAX_TOOL_ROUNDS, execute_tool_with_retry,
    rewrite_if_asked_to_continue, system_prompt_for_workspace,
};
use crate::state::AppState;

const POLL: Duration = Duration::from_millis(400);
/// One official-CLI round. The host kills the process here. `WAIT` is longer
/// so the result can be stored. The chat_turn lease (`chat_turn_lease_for`)
/// is longer than both: the host reclaims expired leases on its 2s poll, and
/// a 2-minute lease used to drop the row while `grok` was still running.
pub const CHAT_CLI_TIMEOUT_S: u64 = 600;
const WAIT_SLACK_S: u64 = 75;
const WAIT: Duration = Duration::from_secs(CHAT_CLI_TIMEOUT_S + WAIT_SLACK_S);
const OFFLINE_AFTER: TimeDelta = TimeDelta::seconds(90);

/// Lease covers the CLI kill plus time to POST complete.
pub fn chat_turn_lease_for(payload: &Value) -> TimeDelta {
    let timeout_s = payload
        .get("timeout_s")
        .and_then(|v| v.as_i64())
        .unwrap_or(CHAT_CLI_TIMEOUT_S as i64)
        .clamp(30, 600);
    TimeDelta::seconds(timeout_s + 90)
}

pub fn chat_turn_deadline_message(provider: &str, host_name: &str, host_online: bool) -> String {
    if host_online {
        format!(
            "{provider} on {host_name} did not finish the chat turn in time. fleet-host is online; the official CLI timed out or its job lease ended before it reported a result."
        )
    } else {
        "runner did not finish the chat turn in time — is fleet-host from this tree running?".into()
    }
}

pub fn runner_cli_provider(provider: &str) -> bool {
    matches!(
        provider,
        "grok" | "xai" | "claude" | "anthropic" | "ollama" | "openai" | "gemini"
    )
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
    user_id: Uuid,
    runner_id: Uuid,
    provider: &str,
    model: &str,
    effort: Option<&str>,
    pairs: &[(String, String)],
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    conversation_id: Option<Uuid>,
) -> AppResult<(String, String)> {
    if chat_turn_is_empty(pairs) {
        return Err(AppError::bad_request("empty chat turn".to_string()));
    }
    let host = require_taking(state, workspace_id, runner_id, user_id).await?;

    let prompt = format_transcript(pairs);
    let claim_id = Uuid::now_v7();
    let max_turns = fleet_db::repos::workspaces::chat_max_turns(&state.db, workspace_id)
        .await
        .unwrap_or(fleet_db::repos::workspaces::CHAT_MAX_TURNS_DEFAULT);
    let payload = json!({
        "kind": "chat_turn",
        "host_id": host.id,
        "provider": provider,
        "model": model,
        "effort": effort,
        "prompt": prompt,
        "timeout_s": CHAT_CLI_TIMEOUT_S,
        "max_turns": max_turns,
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

    wait_for_result(
        &state.db,
        claim_id,
        model,
        progress,
        host.id,
        &host.name,
        provider,
        conversation_id,
    )
    .await
}

const RUNNER_TOOL_PROTOCOL: &str = "\n\
# How to call tools from this runner\n\
The official CLI has no native tool_calls. To use Slack, GitHub, Notion, or fleet,\n\
reply with ONLY this JSON (no prose around it):\n\
{\"tool_calls\":[{\"name\":\"integration_query\",\"arguments\":{\"provider\":\"slack\",\"operation\":\"…\",\"args\":{}}}]}\n\
You may emit several calls in one array. After they run you will get results.\n\
When you are done, reply in plain language — not JSON.\n\
Do not claim a write succeeded unless a tool result has ok:true.\n";

/// LLM on the paired runner; tools (`integration_query`, `launch_agent_plan`, …)
/// stay on the control plane so tokens never leave it.
pub async fn run_tools_on_runner(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    runner_id: Uuid,
    provider: &str,
    model: &str,
    effort: Option<&str>,
    history: &[(String, String)],
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    conversation_id: Option<Uuid>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    let _host = require_taking(state, workspace_id, runner_id, user_id).await?;
    let system = {
        let mut s = system_prompt_for_workspace(state, workspace_id).await;
        s.push_str(RUNNER_TOOL_PROTOCOL);
        s
    };
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(history.len() + 4);
    pairs.push(("system".into(), system));
    pairs.extend(history.iter().cloned());

    let mut actions: Vec<ChatAction> = Vec::new();
    let mut used_model = if model.trim().is_empty() {
        provider.to_string()
    } else {
        model.to_string()
    };
    let mut max_rounds = MAX_TOOL_ROUNDS;
    let mut round = 0usize;

    while round < max_rounds {
        let (reply_model, text) = run_on_runner(
            state,
            workspace_id,
            user_id,
            runner_id,
            provider,
            model,
            effort,
            &pairs,
            progress,
            conversation_id,
        )
        .await?;
        if !reply_model.trim().is_empty() {
            used_model = reply_model;
        }
        let Some(calls) = parse_runner_tool_calls(&text) else {
            let mut content = text.trim().to_string();
            if let Some(rewritten) = rewrite_if_asked_to_continue(&content, &actions) {
                content = rewritten;
            }
            if content.is_empty() && !actions.is_empty() {
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                content = format!("Done.\n{summary}");
            }
            return Ok((used_model, content, actions));
        };

        pairs.push(("assistant".into(), text));
        let llm = ChatLlmContext {
            provider: provider.to_string(),
            model: used_model.clone(),
            runner_id: Some(runner_id),
            effort: effort.map(str::to_string),
        };
        let prior: Vec<Value> = actions.iter().filter_map(|a| a.detail.clone()).collect();
        let mut tool_blob = String::new();
        let n_calls = calls.len();
        for (name, args) in calls {
            emit(
                progress,
                ChatProgress::Status {
                    text: format!("Using {name}…"),
                },
            )
            .await;
            let (result_json, action) =
                execute_tool_with_retry(state, workspace_id, user_id, &name, &args, &llm, &prior)
                    .await;
            emit(progress, ChatProgress::from_action(&action)).await;
            let line = serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into());
            if !tool_blob.is_empty() {
                tool_blob.push('\n');
            }
            tool_blob.push_str(&format!("{name}: {line}"));
            actions.push(action);
        }
        pairs.push((
            "user".into(),
            format!("Tool results (control plane; tokens stayed here):\n{tool_blob}"),
        ));
        let failed: Vec<&ChatAction> = actions
            .iter()
            .rev()
            .take(n_calls)
            .filter(|a| !a.ok)
            .collect();
        if !failed.is_empty() {
            pairs.push(("system".into(), crate::retry::fail_nudge(&failed)));
            let extra = max_rounds - MAX_TOOL_ROUNDS;
            if extra < crate::retry::EXTRA_ROUNDS_ON_FAIL {
                max_rounds += 1;
            }
        }
        round += 1;
    }

    if actions.is_empty() {
        return Err(AppError::bad_request(
            "runner used tools for too many rounds without a final reply".to_string(),
        ));
    }
    let summary = actions
        .iter()
        .map(|a| format!("• {}: {}", a.tool, a.summary))
        .collect::<Vec<_>>()
        .join("\n");
    Ok((used_model, format!("Done.\n{summary}"), actions))
}

/// Pull `tool_calls` out of official-CLI plain text.
pub(crate) fn parse_runner_tool_calls(text: &str) -> Option<Vec<(String, Value)>> {
    let v = extract_json_object(text)?;
    if let Some(arr) = v.get("tool_calls").and_then(|x| x.as_array()) {
        let mut out = Vec::new();
        for c in arr {
            let Some(name) = c
                .get("name")
                .or_else(|| c.get("tool"))
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let args = tool_args_value(
                c.get("arguments")
                    .or_else(|| c.get("args"))
                    .cloned()
                    .unwrap_or(json!({})),
            );
            out.push((name.to_string(), args));
        }
        if out.is_empty() { None } else { Some(out) }
    } else {
        let name = v
            .get("name")
            .or_else(|| v.get("tool"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        if !looks_like_fleet_tool(name) {
            return None;
        }
        let args = tool_args_value(
            v.get("arguments")
                .or_else(|| v.get("args"))
                .cloned()
                .unwrap_or(json!({})),
        );
        Some(vec![(name.to_string(), args)])
    }
}

fn looks_like_fleet_tool(name: &str) -> bool {
    matches!(
        name,
        "integration_query"
            | "list_integrations"
            | "get_integration"
            | "launch_agent_plan"
            | "launch_agent_task"
            | "list_agent_personas"
            | "get_fleet_status"
            | "list_sandboxes"
            | "list_tasks"
            | "list_runs"
            | "read_team_chat"
            | "send_team_message"
            | "monitor_tickets"
            | "list_tickets"
            | "finish_ticket"
            | "start_case"
            | "list_cases"
            | "get_case"
            | "observe_case"
            | "cancel_case"
    )
}

fn tool_args_value(args: Value) -> Value {
    if let Some(s) = args.as_str() {
        serde_json::from_str(s).unwrap_or(json!({}))
    } else if args.is_null() {
        json!({})
    } else {
        args
    }
}

fn extract_json_object(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t)
        && v.is_object()
    {
        return Some(v);
    }
    if let Some(inner) = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```JSON"))
    {
        let inner = inner.strip_prefix('\n').unwrap_or(inner);
        if let Some(end) = inner.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(inner[..end].trim())
                && v.is_object()
            {
                return Some(v);
            }
        }
    }
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &t[start..=end];
    serde_json::from_str(slice).ok().filter(Value::is_object)
}

/// `DELETE` revoke sets `status=disabled` and drops the machine token, but
/// leaves `enabled=true` and a fresh `last_seen_at`. Chat must 400 on that
/// leftover — not enqueue `chat_turn` and wait out the CLI timeout.
pub(crate) fn host_is_removed(host: &WorkerHost) -> bool {
    host.status == fleet_db::WORKER_HOST_STATUS_DISABLED
}

/// Workspace-visible runner that is still taking chat. Call before enqueue
/// and before NDJSON so a leftover `runner_id` 400s instead of hanging.
pub(crate) async fn require_taking(
    state: &AppState,
    workspace_id: Uuid,
    runner_id: Uuid,
    actor: Uuid,
) -> AppResult<WorkerHost> {
    let host = crate::worker_hosts::load_visible_host(state, workspace_id, runner_id).await?;
    if host.is_cluster_host() || host.created_by.is_none() {
        return Err(AppError::bad_request("This runner is not Run on a PC"));
    }
    if !send_policy_allows(&host, actor) {
        return Err(AppError::forbidden(
            "this PC only accepts jobs from its owner",
        ));
    }
    ensure_online(&host)?;
    Ok(host)
}

pub(crate) fn send_policy_allows(host: &WorkerHost, actor: Uuid) -> bool {
    match host.send_policy.as_str() {
        fleet_db::WORKER_HOST_SEND_WORKSPACE => true,
        _ => host.created_by.map(|o| o == actor).unwrap_or(false),
    }
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
    // of enqueueing a turn the box will not poll.
    if host.software_updating {
        return Err(AppError::bad_request(format!(
            "runner `{}` is updating / not taking jobs (host is updating)",
            host.name
        )));
    }
    // v1 is host mode only. allow_host=false or advertised_host=false already
    // makes /me/lease claim:null; chat must 400 here instead of enqueueing.
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

fn claim_progress(payload: &Value) -> String {
    payload
        .get("progress")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

async fn host_recently_seen(pool: &fleet_db::PgPool, host_id: Uuid) -> bool {
    match fleet_db::repos::worker_hosts::get(pool, host_id).await {
        Ok(Some(host)) => host
            .last_seen_at
            .is_some_and(|seen| Utc::now() - seen <= OFFLINE_AFTER),
        _ => false,
    }
}

async fn wait_for_result(
    pool: &fleet_db::PgPool,
    claim_id: Uuid,
    fallback_model: &str,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    host_id: Uuid,
    host_name: &str,
    provider: &str,
    _conversation_id: Option<Uuid>,
) -> AppResult<(String, String)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let started = tokio::time::Instant::now();
    let mut last_tick = 0u64;
    let mut last_progress = String::new();
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
        let noted = claim_progress(&claim.payload);
        if !noted.is_empty() && noted != last_progress {
            last_progress = noted.clone();
            emit(
                progress,
                ChatProgress::Status {
                    text: noted.clone(),
                },
            )
            .await;
        }
        if tokio::time::Instant::now() >= deadline {
            let online = host_recently_seen(pool, host_id).await;
            return Err(AppError::bad_request(chat_turn_deadline_message(
                provider, host_name, online,
            )));
        }
        let secs = started.elapsed().as_secs();
        if secs >= 5 && secs / 5 > last_tick {
            last_tick = secs / 5;
            let text = if last_progress.is_empty() {
                format!("Running {provider} on {host_name}… {secs}s")
            } else {
                format!("{last_progress} · {secs}s")
            };
            emit(progress, ChatProgress::Status { text }).await;
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
            t, "User:\nhi\n\nAssistant:\nhello",
            "two-turn CLI prompt must keep User:/Assistant: order and a blank-line separator"
        );
    }

    #[test]
    fn format_transcript_trims_pair_content() {
        let t = format_transcript(&[("user".into(), "  hi\nthere  ".into())]);
        assert_eq!(
            t, "User:\nhi\nthere",
            "format_transcript must trim pair content (resumed DB history is not trimmed first)"
        );
    }

    /// Done claim whose CLI text is empty or whitespace must 400 (do not persist
    /// an empty assistant turn). Empty HTTP `messages` is locked in chat_stream_http.
    #[test]
    fn parse_result_empty_or_whitespace_cli_text_is_bad_request() {
        for (label, payload) in [
            (
                "whitespace",
                json!({ "result": { "ok": true, "text": "  " } }),
            ),
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
            (
                "absent",
                json!({ "result": { "ok": true, "text": "hello" } }),
            ),
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
            runner_kind: fleet_db::RUNNER_KIND_SELF_HOSTED.into(),
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
            send_policy: fleet_db::WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            run_mode: fleet_db::WORKER_HOST_RUN_ON_PC.to_string(),
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
        let err = ensure_online(&host)
            .expect_err("ensure_online must fail on DELETE leftover before last_seen/enabled");
        assert!(
            matches!(&err, AppError::BadRequest(m) if m.contains("removed")),
            "first ensure_online branch is host_is_removed, not offline/not-taking: {err:?}"
        );
    }

    #[test]
    fn parses_tool_calls_from_plain_cli_text() {
        let raw = r#"{"tool_calls":[{"name":"integration_query","arguments":{"provider":"slack","operation":"list_channels"}}]}"#;
        let calls = parse_runner_tool_calls(raw).expect("json object");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "integration_query");
        assert_eq!(calls[0].1["provider"], "slack");

        let fenced = "```json\n{\"tool_calls\":[{\"name\":\"launch_agent_plan\",\"arguments\":{\"title\":\"do it\",\"tasks\":[]}}]}\n```";
        let calls = parse_runner_tool_calls(fenced).expect("fence");
        assert_eq!(calls[0].0, "launch_agent_plan");

        let single = r#"Sure — {"name":"integration_query","arguments":{"provider":"notion","operation":"append_text"}}"#;
        let calls = parse_runner_tool_calls(single).expect("embedded");
        assert_eq!(calls[0].0, "integration_query");
        assert_eq!(calls[0].1["provider"], "notion");

        assert!(parse_runner_tool_calls("I'll read Slack and write Notion.").is_none());
        assert!(parse_runner_tool_calls(r#"{"name":"not_a_fleet_tool","arguments":{}}"#).is_none());
    }

    #[test]
    fn chat_turn_lease_outlives_cli_timeout() {
        let lease = chat_turn_lease_for(&json!({ "timeout_s": 600 }));
        assert_eq!(
            lease.num_seconds(),
            690,
            "lease must outlive the 600s CLI kill so complete still holds the token"
        );
        let from_default = chat_turn_lease_for(&json!({}));
        assert_eq!(from_default.num_seconds(), 690);
        let short = chat_turn_lease_for(&json!({ "timeout_s": 30 }));
        assert_eq!(short.num_seconds(), 120);
        let clamped = chat_turn_lease_for(&json!({ "timeout_s": 9_999 }));
        assert_eq!(clamped.num_seconds(), 690);
    }

    #[test]
    fn deadline_message_names_a_live_host() {
        let offline = chat_turn_deadline_message("grok", "laptop", false);
        assert!(
            offline.contains("is fleet-host from this tree running?"),
            "{offline}"
        );
        let online = chat_turn_deadline_message("grok", "laptop", true);
        assert!(
            online.contains("fleet-host is online"),
            "a heartbeating host must not be described as down: {online}"
        );
        assert!(
            !online.contains("is fleet-host from this tree running?"),
            "{online}"
        );
    }

    #[test]
    fn claim_progress_reads_host_note() {
        assert_eq!(
            claim_progress(&json!({ "progress": " Using run_terminal_command… " })),
            "Using run_terminal_command…"
        );
        assert_eq!(claim_progress(&json!({})), "");
    }
}
