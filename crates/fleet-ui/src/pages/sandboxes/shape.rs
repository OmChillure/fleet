//! Worker-session list shape used by the Sandboxes page.
//!
//! GET `/fleet/workers` is GET `/fleet/sessions` with the array key renamed
//! `sessions` → `sandboxes`. Row identity is the session id.

use serde::Deserialize;
use serde_json::Value;

pub const SESSIONS_ARRAY_KEY: &str = "sessions";
pub const SANDBOXES_ARRAY_KEY: &str = "sandboxes";
pub const BACKEND_KEY: &str = "sandbox_backend";

pub const PAGE_HEADING: &str = "Sandboxes";
pub const PAGE_SUBTITLE: &str =
    "Worker sessions spawned by fleet ticks (chat launch or Fleet fixture).";
pub const EMPTY_HEADING: &str = "No sandboxes yet";
pub const EMPTY_HINT: &str = "From Chat, ask the assistant to launch work in a sandbox, or use Fleet → Start 2-task run → Run tick.";
pub const LOAD_ERROR: &str =
    "Could not load sandboxes. Check API connectivity and workspace membership.";
pub const DEFAULT_BACKEND: &str = "stub";
pub const KILL_REASON: &str = "ui_kill";
pub const KILL_LABEL: &str = "Kill";

const LIVE_STATUSES: &[&str] = &["spawning", "healthy", "stuck", "killing"];

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SandboxRow {
    pub id: String,
    pub run_id: String,
    pub board_task_id: String,
    #[serde(default)]
    pub attempt: i64,
    pub status: String,
    #[serde(default)]
    pub runtime_backend: String,
    #[serde(default)]
    pub runtime_handle: Option<String>,
    #[serde(default)]
    pub worker_profile: String,
    #[serde(default)]
    pub progress_pct: i64,
    #[serde(default)]
    pub work_dir: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub exited_at: Option<String>,
    #[serde(default)]
    pub exit_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SandboxesResponse {
    #[serde(default)]
    pub sandbox_backend: String,
    #[serde(default)]
    pub sandboxes: Vec<SandboxRow>,
}

impl SandboxesResponse {
    pub fn display_backend(&self) -> String {
        default_sandbox_backend(&self.sandbox_backend)
    }
}

/// What one sandbox card renders from a session-shaped row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCardModel {
    pub status: String,
    pub pill_class: &'static str,
    pub worker_profile: String,
    pub runtime_backend: String,
    pub progress_label: String,
    pub session_short: String,
    pub run_short: String,
    pub task_short: String,
    pub attempt: i64,
    pub started: String,
    pub exited: String,
    pub exit_reason: Option<String>,
    pub work_dir: Option<String>,
    pub show_kill: bool,
}

pub fn list_sandboxes_path(workspace_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/workers")
}

pub fn list_sessions_path(workspace_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/sessions")
}

pub fn kill_sandbox_path(workspace_id: &str, session_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill")
}

pub fn default_sandbox_backend(raw: &str) -> String {
    if raw.is_empty() {
        DEFAULT_BACKEND.into()
    } else {
        raw.into()
    }
}

pub fn list_status_line(n: usize) -> String {
    format!("{n} sandbox session(s)")
}

pub fn short_id(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

pub fn format_ts(ts: &Option<String>) -> String {
    match ts {
        Some(s) if !s.is_empty() => s.get(..19).unwrap_or(s).replace('T', " "),
        _ => "—".into(),
    }
}

pub fn status_pill_class(status: &str) -> &'static str {
    match status {
        "healthy" | "spawning" => "bg-sky-500/15 text-sky-400 border-sky-500/30",
        "stuck" | "killing" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        "exited" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "failed_spawn" => "bg-red-500/15 text-red-400 border-red-500/30",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

pub fn sandbox_is_live(status: &str) -> bool {
    LIVE_STATUSES.contains(&status)
}

pub fn sandbox_card_model(row: &SandboxRow) -> SandboxCardModel {
    SandboxCardModel {
        status: row.status.clone(),
        pill_class: status_pill_class(&row.status),
        worker_profile: row.worker_profile.clone(),
        runtime_backend: row.runtime_backend.clone(),
        progress_label: format!("{}%", row.progress_pct),
        session_short: short_id(&row.id),
        run_short: short_id(&row.run_id),
        task_short: short_id(&row.board_task_id),
        attempt: row.attempt,
        started: format_ts(&row.started_at),
        exited: format_ts(&row.exited_at),
        exit_reason: nonempty_opt(&row.exit_reason),
        work_dir: nonempty_opt(&row.work_dir),
        show_kill: false,
    }
}

pub fn parse_sandbox_row(value: &Value) -> Result<SandboxRow, serde_json::Error> {
    serde_json::from_value(value.clone())
}

pub fn parse_sandboxes_response(value: &Value) -> Result<SandboxesResponse, serde_json::Error> {
    serde_json::from_value(value.clone())
}

/// GET `/fleet/sessions` body as the sandboxes page would show it.
pub fn sandboxes_from_sessions_envelope(
    sessions_body: &Value,
) -> Result<SandboxesResponse, String> {
    let backend = sessions_body
        .get(BACKEND_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let sessions = sessions_body
        .get(SESSIONS_ARRAY_KEY)
        .and_then(|v| v.as_array())
        .ok_or_else(|| "sessions list missing `sessions` array".to_string())?;
    let mut sandboxes = Vec::with_capacity(sessions.len());
    for item in sessions {
        sandboxes.push(
            parse_sandbox_row(item)
                .map_err(|e| format!("session item is not a sandbox row: {e}"))?,
        );
    }
    Ok(SandboxesResponse {
        sandbox_backend: default_sandbox_backend(backend),
        sandboxes,
    })
}

fn nonempty_opt(value: &Option<String>) -> Option<String> {
    value.clone().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // catalog:unit-sandboxes-page — workers.sandboxes is the sessions list.
    // Host: cargo test -p fleet-ui --locked -- sandboxes
    // Layer-2 page lock (just test excludes fleet-ui; do not use -k sandboxes):
    //   python3 -m pytest tests/browser/test_app_e2e.py -q -k sandbox

    const SID_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const RUN_A: &str = "11111111-1111-4111-8111-111111111111";
    const TASK_A: &str = "33333333-3333-4333-8333-333333333333";

    fn session_item(
        id: &str,
        run_id: &str,
        board_task_id: &str,
        status: &str,
        attempt: i64,
        progress: i64,
    ) -> Value {
        json!({
            "id": id,
            "run_id": run_id,
            "board_task_id": board_task_id,
            "attempt": attempt,
            "status": status,
            "runtime_backend": "process",
            "runtime_handle": "handle-1",
            "worker_profile": "coding",
            "progress_pct": progress,
            "last_heartbeat_at": "2026-08-25T12:00:00.123456Z",
            "exit_reason": null,
            "work_dir": "/tmp/work/s1",
            "started_at": "2026-08-25T12:00:00.123456Z",
            "exited_at": null,
            "created_at": "2026-08-25T11:59:00Z",
        })
    }

    #[test]
    fn card_model_exposes_session_fields() {
        let row = parse_sandbox_row(&session_item(SID_A, RUN_A, TASK_A, "healthy", 3, 42)).unwrap();
        let card = sandbox_card_model(&row);
        assert_eq!(card.progress_label, "42%");
        assert_eq!(card.session_short, "aaaaaaaa");
        assert_eq!(card.run_short, "11111111");
        assert_eq!(card.task_short, "33333333");
        assert_eq!(card.started, "2026-08-25 12:00:00");
        assert_eq!(card.exited, "—");
        assert_eq!(card.work_dir.as_deref(), Some("/tmp/work/s1"));
    }

    #[test]
    fn exited_card_shows_reason_and_timestamp() {
        let mut item = session_item(SID_A, RUN_A, TASK_A, "exited", 1, 100);
        item["exit_reason"] = json!("ui_kill");
        item["exited_at"] = json!("2026-08-25T12:05:00Z");
        item["work_dir"] = json!("");
        let card = sandbox_card_model(&parse_sandbox_row(&item).unwrap());
        assert_eq!(card.exit_reason.as_deref(), Some("ui_kill"));
        assert_eq!(card.exited, "2026-08-25 12:05:00");
        assert_eq!(card.work_dir, None);
    }
}
