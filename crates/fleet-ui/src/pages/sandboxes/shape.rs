//! Worker-session list shape used by the Sandboxes page.
//!
//! GET `/fleet/workers` is GET `/fleet/sessions` with the array key renamed
//! `sessions` → `sandboxes`. Row identity is the session id.

use serde::Deserialize;
use serde_json::Value;

/// JSON keys on each GET `/fleet/sessions` item and each GET `/fleet/workers` sandbox.
pub const SHARED_SESSION_SANDBOX_KEYS: &[&str] = &[
    "id",
    "run_id",
    "board_task_id",
    "attempt",
    "status",
    "runtime_backend",
    "runtime_handle",
    "worker_profile",
    "progress_pct",
    "work_dir",
    "started_at",
    "exited_at",
    "exit_reason",
    "created_at",
];

/// Present on both HTTP lists; the card does not render it.
pub const SESSION_EXTRA_KEYS: &[&str] = &["last_heartbeat_at"];

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

pub fn row_ids(rows: &[SandboxRow]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_str()).collect()
}

pub fn json_ids(array_key: &str, body: &Value) -> Vec<String> {
    body.get(array_key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn nonempty_opt(value: &Option<String>) -> Option<String> {
    value.clone().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // catalog:unit-sandboxes-page — workers.sandboxes is the sessions list.
    // catalog:list-sandboxes-page-no-destroy-button — Docs §6.6 status-only cards.
    // Host: cargo test -p fleet-ui --locked -- sandboxes
    // Layer-2 page lock (just test excludes fleet-ui; do not use -k sandboxes):
    //   python3 -m pytest tests/browser/test_app_e2e.py -q -k sandbox

    const SID_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const SID_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const RUN_A: &str = "11111111-1111-4111-8111-111111111111";
    const RUN_B: &str = "22222222-2222-4222-8222-222222222222";
    const TASK_A: &str = "33333333-3333-4333-8333-333333333333";
    const TASK_B: &str = "44444444-4444-4444-8444-444444444444";
    const WS: &str = "7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90";

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

    fn sessions_envelope(items: Vec<Value>) -> Value {
        json!({
            "sandbox_backend": "process",
            "sessions": items,
        })
    }

    fn workers_envelope(items: Vec<Value>) -> Value {
        json!({
            "sandbox_backend": "process",
            "sandboxes": items,
        })
    }

    #[test]
    fn session_item_deserializes_as_sandbox_row() {
        let item = session_item(SID_A, RUN_A, TASK_A, "healthy", 1, 40);
        for key in SHARED_SESSION_SANDBOX_KEYS {
            assert!(item.get(*key).is_some(), "session item missing {key}");
        }
        for key in SESSION_EXTRA_KEYS {
            assert!(item.get(*key).is_some(), "session item missing extra {key}");
        }
        let row = parse_sandbox_row(&item).expect("session-shaped JSON is a sandbox row");
        assert_eq!(row.id, SID_A);
        assert_eq!(row.run_id, RUN_A);
        assert_eq!(row.board_task_id, TASK_A);
        assert_eq!(row.attempt, 1);
        assert_eq!(row.status, "healthy");
        assert_eq!(row.runtime_backend, "process");
        assert_eq!(row.runtime_handle.as_deref(), Some("handle-1"));
        assert_eq!(row.worker_profile, "coding");
        assert_eq!(row.progress_pct, 40);
        assert_eq!(row.work_dir.as_deref(), Some("/tmp/work/s1"));
        assert_eq!(
            row.started_at.as_deref(),
            Some("2026-08-25T12:00:00.123456Z")
        );
        assert_eq!(row.exited_at, None);
        assert_eq!(row.exit_reason, None);
        assert_eq!(row.created_at.as_deref(), Some("2026-08-25T11:59:00Z"));
    }

    #[test]
    fn workers_envelope_deserializes_same_rows_as_sessions() {
        let a = session_item(SID_A, RUN_A, TASK_A, "healthy", 1, 40);
        let b = session_item(SID_B, RUN_B, TASK_B, "exited", 2, 100);
        let sessions = sessions_envelope(vec![a.clone(), b.clone()]);
        let workers = workers_envelope(vec![a, b]);

        let from_sessions =
            sandboxes_from_sessions_envelope(&sessions).expect("sessions envelope remaps");
        let from_workers =
            parse_sandboxes_response(&workers).expect("workers envelope is SandboxesResponse");

        assert_eq!(
            json_ids(SESSIONS_ARRAY_KEY, &sessions),
            json_ids(SANDBOXES_ARRAY_KEY, &workers)
        );
        assert_eq!(
            row_ids(&from_sessions.sandboxes),
            row_ids(&from_workers.sandboxes)
        );
        assert_eq!(from_sessions.sandboxes, from_workers.sandboxes);
        assert_eq!(from_workers.sandboxes[0].id, SID_A);
        assert_eq!(from_workers.sandboxes[1].id, SID_B);
        assert_eq!(from_workers.display_backend(), "process");
    }

    #[test]
    fn workers_sandbox_ids_match_session_ids_in_order() {
        let items = vec![
            session_item(SID_B, RUN_B, TASK_B, "stuck", 1, 10),
            session_item(SID_A, RUN_A, TASK_A, "spawning", 1, 0),
        ];
        let sessions = sessions_envelope(items.clone());
        let workers = workers_envelope(items);
        assert_eq!(
            json_ids(SANDBOXES_ARRAY_KEY, &workers),
            json_ids(SESSIONS_ARRAY_KEY, &sessions)
        );
        assert_eq!(
            json_ids(SANDBOXES_ARRAY_KEY, &workers),
            vec![SID_B.to_string(), SID_A.to_string()]
        );
    }

    #[test]
    fn empty_sessions_is_empty_sandboxes() {
        let sessions = sessions_envelope(vec![]);
        let workers = workers_envelope(vec![]);
        let from_sessions = sandboxes_from_sessions_envelope(&sessions).unwrap();
        let from_workers = parse_sandboxes_response(&workers).unwrap();
        assert!(from_sessions.sandboxes.is_empty());
        assert!(from_workers.sandboxes.is_empty());
        assert_eq!(list_status_line(0), "0 sandbox session(s)");
        assert_eq!(list_status_line(2), "2 sandbox session(s)");
    }

    #[test]
    fn empty_backend_displays_as_stub() {
        let workers = json!({ "sandbox_backend": "", "sandboxes": [] });
        let resp = parse_sandboxes_response(&workers).unwrap();
        assert_eq!(resp.sandbox_backend, "");
        assert_eq!(resp.display_backend(), DEFAULT_BACKEND);
        assert_eq!(default_sandbox_backend(""), "stub");
        assert_eq!(default_sandbox_backend("docker"), "docker");
    }

    #[test]
    fn missing_optional_session_fields_default() {
        let minimal = json!({
            "id": SID_A,
            "run_id": RUN_A,
            "board_task_id": TASK_A,
            "status": "healthy",
        });
        let row = parse_sandbox_row(&minimal).expect("required session keys only");
        assert_eq!(row.attempt, 0);
        assert_eq!(row.progress_pct, 0);
        assert_eq!(row.runtime_backend, "");
        assert_eq!(row.runtime_handle, None);
        assert_eq!(row.worker_profile, "");
        assert_eq!(row.work_dir, None);
        assert_eq!(row.started_at, None);
        assert_eq!(row.exited_at, None);
        assert_eq!(row.exit_reason, None);
        assert_eq!(row.created_at, None);
    }

    #[test]
    fn missing_required_session_id_fails() {
        let bad = json!({
            "run_id": RUN_A,
            "board_task_id": TASK_A,
            "status": "healthy",
        });
        assert!(parse_sandbox_row(&bad).is_err());
    }

    #[test]
    fn chat_history_item_is_not_a_sandbox_row() {
        let chat = json!({
            "id": SID_A,
            "title": "Hello",
            "preview": "hi",
            "message_count": 3,
            "updated_at": "2026-08-25T12:00:00Z",
        });
        assert!(
            parse_sandbox_row(&chat).is_err(),
            "UI /sessions chat history must not parse as a worker sandbox"
        );
        assert!(sandboxes_from_sessions_envelope(&chat).is_err());
    }

    #[test]
    fn last_heartbeat_at_does_not_break_parse() {
        let mut item = session_item(SID_A, RUN_A, TASK_A, "healthy", 1, 5);
        item["last_heartbeat_at"] = json!("2026-08-25T12:34:56Z");
        item["unknown_future_field"] = json!("ignored");
        let row = parse_sandbox_row(&item).expect("extra session keys are ignored");
        assert_eq!(row.id, SID_A);
    }

    #[test]
    fn list_path_is_workers_not_sessions() {
        let workers = list_sandboxes_path(WS);
        let sessions = list_sessions_path(WS);
        assert_eq!(workers, format!("/api/workspaces/{WS}/fleet/workers"));
        assert_eq!(sessions, format!("/api/workspaces/{WS}/fleet/sessions"));
        assert_ne!(workers, sessions);
        assert!(!workers.contains("/sessions"));
        assert!(sessions.ends_with("/sessions"));
    }

    #[test]
    fn kill_path_uses_session_id() {
        let path = kill_sandbox_path(WS, SID_A);
        assert_eq!(
            path,
            format!("/api/workspaces/{WS}/fleet/sessions/{SID_A}/kill")
        );
        assert!(path.contains(SID_A));
        assert_eq!(KILL_REASON, "ui_kill");
        assert_eq!(KILL_LABEL, "Kill");
    }

    #[test]
    fn card_model_exposes_session_fields() {
        let row = parse_sandbox_row(&session_item(SID_A, RUN_A, TASK_A, "healthy", 3, 42)).unwrap();
        let card = sandbox_card_model(&row);
        assert_eq!(card.status, "healthy");
        assert_eq!(card.worker_profile, "coding");
        assert_eq!(card.runtime_backend, "process");
        assert_eq!(card.progress_label, "42%");
        assert_eq!(card.session_short, "aaaaaaaa");
        assert_eq!(card.run_short, "11111111");
        assert_eq!(card.task_short, "33333333");
        assert_eq!(card.attempt, 3);
        assert_eq!(card.started, "2026-08-25 12:00:00");
        assert_eq!(card.exited, "—");
        assert_eq!(card.exit_reason, None);
        assert_eq!(card.work_dir.as_deref(), Some("/tmp/work/s1"));
        assert!(
            !card.show_kill,
            "docs §6.6: healthy card is status-only, no Kill"
        );
        assert_eq!(card.pill_class, status_pill_class("healthy"));
    }

    #[test]
    fn live_statuses_are_not_a_kill_button() {
        // Capacity still counts live sessions. Kill is not this page (§6.6 / §17).
        for status in LIVE_STATUSES {
            assert!(sandbox_is_live(status), "{status} is live");
            let row = parse_sandbox_row(&session_item(SID_A, RUN_A, TASK_A, status, 1, 1)).unwrap();
            assert!(
                !sandbox_card_model(&row).show_kill,
                "{status} card must not offer Kill"
            );
        }
        for status in ["exited", "failed_spawn", "failed", "cancelled", ""] {
            assert!(!sandbox_is_live(status), "{status} is not live");
            let row =
                parse_sandbox_row(&session_item(SID_A, RUN_A, TASK_A, status, 1, 100)).unwrap();
            assert!(
                !sandbox_card_model(&row).show_kill,
                "{status} card must not offer Kill"
            );
        }
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
        assert!(!card.show_kill);
    }

    #[test]
    fn short_id_and_format_ts_edges() {
        assert_eq!(short_id("abcdef12xxxx"), "abcdef12");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
        assert_eq!(format_ts(&None), "—");
        assert_eq!(format_ts(&Some(String::new())), "—");
        assert_eq!(
            format_ts(&Some("2026-08-25T12:00:00.999Z".into())),
            "2026-08-25 12:00:00"
        );
        assert_eq!(format_ts(&Some("short".into())), "short");
    }

    #[test]
    fn status_pills_group_known_session_statuses() {
        assert_eq!(status_pill_class("healthy"), status_pill_class("spawning"));
        assert_eq!(status_pill_class("stuck"), status_pill_class("killing"));
        assert_ne!(
            status_pill_class("exited"),
            status_pill_class("failed_spawn")
        );
        assert_ne!(status_pill_class("healthy"), status_pill_class("exited"));
        assert_eq!(status_pill_class("unknown"), status_pill_class(""));
        assert_eq!(
            status_pill_class("failed_spawn"),
            "bg-red-500/15 text-red-400 border-red-500/30"
        );
    }

    #[test]
    fn page_copy_names_sessions_as_sandboxes() {
        assert_eq!(PAGE_HEADING, "Sandboxes");
        assert!(PAGE_SUBTITLE.contains("Worker sessions"));
        assert_eq!(EMPTY_HEADING, "No sandboxes yet");
        assert!(EMPTY_HINT.contains("Fleet → Start 2-task run → Run tick"));
        assert!(LOAD_ERROR.contains("workspace membership"));
    }

    #[test]
    fn remapping_sessions_without_array_fails_closed() {
        let workers_shaped = json!({ "sandbox_backend": "stub", "sandboxes": [] });
        assert!(
            sandboxes_from_sessions_envelope(&workers_shaped).is_err(),
            "workers envelope is not a sessions envelope"
        );
        assert!(sandboxes_from_sessions_envelope(&json!({})).is_err());
    }

    #[test]
    fn page_rsx_binds_sandboxes_copy_and_session_cards() {
        let page = include_str!("page.rs");
        let api = include_str!("api.rs");
        assert!(page.contains("{PAGE_HEADING}"));
        assert!(page.contains("{PAGE_SUBTITLE}"));
        assert!(page.contains("{EMPTY_HEADING}"));
        assert!(page.contains("{EMPTY_HINT}"));
        assert!(page.contains("list_status_line"));
        assert!(page.contains("whitespace-pre"));
        assert!(page.contains("session  {card.session_short}"));
        assert!(page.contains("run      {card.run_short}"));
        assert!(page.contains("task     {card.task_short}"));
        assert!(page.contains("{card.status}"));
        assert!(page.contains("list_sandboxes"));
        assert!(
            api.contains("list_sandboxes_path"),
            "list client must GET /fleet/workers, not /fleet/sessions"
        );
        assert!(
            !page.contains("Chat history"),
            "Sandboxes RSX must not render Chat history"
        );
        assert_page_has_no_destroy_or_kill_button(page);
    }

    #[test]
    fn page_rsx_has_no_destroy_or_kill_button() {
        // catalog:list-sandboxes-page-no-destroy-button
        // Docs §6.6: status pill / profile / backend / progress / ids only.
        // Docs §17: destroy/kill from the UI is a gap (cancel the run).
        assert_page_has_no_destroy_or_kill_button(include_str!("page.rs"));
    }

    fn assert_page_has_no_destroy_or_kill_button(page: &str) {
        for needle in [
            "KILL_LABEL",
            "kill_sandbox",
            "show_kill",
            "Destroy",
            "\"Kill\"",
            "'Kill'",
        ] {
            assert!(
                !page.contains(needle),
                "docs §6.6: sandboxes cards are status-only; page.rs must not contain {needle}"
            );
        }
        assert!(
            page.contains("{card.status}"),
            "status-only card must still render the status pill"
        );
        assert!(
            page.contains("session  {card.session_short}"),
            "status-only card must still render session short id"
        );
    }
}
