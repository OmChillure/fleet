//! Shared “who is doing what” board for a run.
//! Each sandbox gets TEAM.md + handoff/ from finished teammates.
//! Durable copies live under `DATA_DIR/projects/{id}/runs/{run_id}/handoffs/`
//! so wipe can delete session dirs without losing the next agent’s input.

use std::path::{Path, PathBuf};

use crate::store::{BoardSnapshot, BoardTaskRow};

const EXCERPT_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub plan_task_id: String,
    pub title: String,
    pub worker_profile: String,
    pub status: String,
    pub depends_on: Vec<String>,
    pub result_excerpt: Option<String>,
}

pub fn run_handoffs_dir(data_dir: &Path, project_id: uuid::Uuid, run_id: uuid::Uuid) -> PathBuf {
    data_dir
        .join("projects")
        .join(project_id.to_string())
        .join("runs")
        .join(run_id.to_string())
        .join("handoffs")
}

pub fn session_dir(data_dir: &Path, project_id: uuid::Uuid, session_id: uuid::Uuid) -> PathBuf {
    data_dir
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(session_id.to_string())
}

pub fn render_team_md(you: &str, members: &[TeamMember]) -> String {
    let mut out = String::from("# Fleet team — who is doing what\n\n");
    if let Some(me) = members.iter().find(|m| m.plan_task_id == you) {
        out.push_str(&format!(
            "You are **`{}`** — persona `{}`.\n**Your job:** {}\n**Your status:** {}\n\n",
            me.plan_task_id,
            me.worker_profile,
            me.title,
            human_status(&me.status)
        ));
        if !me.depends_on.is_empty() {
            out.push_str(&format!(
                "You wait on / use output from: `{}`.\n\n",
                me.depends_on.join("`, `")
            ));
        }
    } else {
        out.push_str(&format!("You are task `{you}`.\n\n"));
    }

    out.push_str("## Teammates\n\n");
    out.push_str("| id | persona | doing | status |\n| --- | --- | --- | --- |\n");
    for m in members {
        let mark = if m.plan_task_id == you {
            " ← you"
        } else {
            ""
        };
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {}{} |\n",
            m.plan_task_id,
            m.worker_profile,
            one_line(&m.title),
            human_status(&m.status),
            mark
        ));
    }
    out.push('\n');

    let handoffs: Vec<&TeamMember> = members
        .iter()
        .filter(|m| m.plan_task_id != you && m.result_excerpt.is_some())
        .collect();
    if !handoffs.is_empty() {
        out.push_str("## Handoffs from finished teammates\n\n");
        out.push_str("Full files also under `handoff/<id>.md`.\n\n");
        for m in handoffs {
            let excerpt = m.result_excerpt.as_deref().unwrap_or("");
            out.push_str(&format!(
                "### `{}` — {}\n\n{}\n\n",
                m.plan_task_id, m.title, excerpt
            ));
        }
    }

    out.push_str(
        "## How to work together\n\
         - Do **only your** task. Do not redo a teammate’s job.\n\
         - **Talk to live teammates.** Use `send_message` (to=`t1` or to=`*` for everyone) and `ask_teammate` when you need a contract, path, or decision *now*. They see it on their next thinking round.\n\
         - If someone asks you a question, reply with `send_message` and `reply_to` set to their message id. Do not ignore asks.\n\
         - **Shared artifacts live in `work/`.** That is the same folder the whole team is building. Read it before writing. Dependents inherit those files.\n\
         - Reviewers: if the work is not good enough, call `request_changes` (they get sent back with your notes). Do not rubber-stamp.\n\
         - If someone is `in progress`, ask them — do not silently redo their slice.\n\
         - If you depend on others, read `work/`, `handoff/`, and `TEAM_CHAT.md` before continuing.\n\
         - End with `result.md` so the next specialist can pick up.\n\
         - Re-read `TEAM.md`; the control plane refreshes it as statuses change.\n",
    );
    out
}

fn human_status(s: &str) -> &'static str {
    match s {
        "queued" => "queued",
        "ready" => "ready to start",
        "assigned" | "in_progress" => "in progress",
        "done_success" => "done",
        "done_failed" => "failed",
        "blocked" => "blocked",
        "retry_wait" => "retrying",
        "cancelled" => "cancelled",
        _ => "unknown",
    }
}

fn one_line(s: &str) -> String {
    s.replace('|', "/")
        .chars()
        .take(80)
        .collect::<String>()
        .replace('\n', " ")
}

fn excerpt_text(raw: &str) -> String {
    let t = raw.trim();
    if t.chars().count() <= EXCERPT_CHARS {
        t.to_string()
    } else {
        let cut: String = t.chars().take(EXCERPT_CHARS).collect();
        format!("{cut}\n…(truncated)")
    }
}

pub fn members_from_snapshot(
    snap: &BoardSnapshot,
    run_id: uuid::Uuid,
    data_dir: &Path,
) -> Vec<TeamMember> {
    let mut tasks: Vec<&BoardTaskRow> = snap.tasks.iter().filter(|t| t.run_id == run_id).collect();
    tasks.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.created_at.cmp(&b.created_at))
    });

    let id_to_plan: std::collections::HashMap<uuid::Uuid, String> = tasks
        .iter()
        .map(|t| (t.id, t.plan_task_id.clone()))
        .collect();

    tasks
        .iter()
        .map(|t| {
            let depends_on = t
                .depends_on
                .iter()
                .filter_map(|id| id_to_plan.get(id).cloned())
                .collect();
            let result_excerpt = if t.status == "done_success" {
                read_handoff_or_session(data_dir, t, snap)
            } else {
                None
            };
            TeamMember {
                plan_task_id: t.plan_task_id.clone(),
                title: t.title.clone(),
                worker_profile: t.worker_profile.clone(),
                status: t.status.clone(),
                depends_on,
                result_excerpt,
            }
        })
        .collect()
}

fn read_handoff_or_session(
    data_dir: &Path,
    task: &BoardTaskRow,
    snap: &BoardSnapshot,
) -> Option<String> {
    let durable = run_handoffs_dir(data_dir, task.project_id, task.run_id)
        .join(format!("{}.md", task.plan_task_id));
    if let Ok(s) = std::fs::read_to_string(&durable) {
        if !s.trim().is_empty() {
            return Some(excerpt_text(&s));
        }
    }
    let session = snap
        .sessions
        .iter()
        .filter(|s| s.board_task_id == task.id)
        .max_by_key(|s| s.created_at)?;
    let path = session_dir(data_dir, task.project_id, session.id).join("result.md");
    std::fs::read_to_string(path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| excerpt_text(&s))
}

pub async fn archive_session_result(
    data_dir: &Path,
    project_id: uuid::Uuid,
    run_id: uuid::Uuid,
    plan_task_id: &str,
    session_work: &Path,
) {
    let src = session_work.join("result.md");
    let Ok(body) = tokio::fs::read_to_string(&src).await else {
        return;
    };
    if body.trim().is_empty() {
        return;
    }
    let dir = run_handoffs_dir(data_dir, project_id, run_id);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let dest = dir.join(format!("{plan_task_id}.md"));
    let _ = tokio::fs::write(dest, body).await;
}

/// Copy durable handoffs this task depends on into `work_dir/handoff/`.
pub async fn copy_dep_handoffs(
    work_dir: &Path,
    you: &TeamMember,
    data_dir: &Path,
    task: &BoardTaskRow,
) {
    if you.depends_on.is_empty() {
        return;
    }
    let src_dir = run_handoffs_dir(data_dir, task.project_id, task.run_id);
    let dest_dir = work_dir.join("handoff");
    let _ = tokio::fs::create_dir_all(&dest_dir).await;
    for id in &you.depends_on {
        let src = src_dir.join(format!("{id}.md"));
        if let Ok(body) = tokio::fs::read_to_string(&src).await {
            let _ = tokio::fs::write(dest_dir.join(format!("{id}.md")), body).await;
        }
    }
}

pub async fn write_team_md(work_dir: &Path, you_plan_id: &str, members: &[TeamMember]) {
    let md = render_team_md(you_plan_id, members);
    let _ = tokio::fs::write(work_dir.join("TEAM.md"), md).await;
}

/// Called at spawn: TEAM.md + any available handoffs from finished deps.
pub async fn prepare_agent_workspace(
    data_dir: &Path,
    work_dir: &Path,
    task: &BoardTaskRow,
    snap: &BoardSnapshot,
) {
    let members = members_from_snapshot(snap, task.run_id, data_dir);
    let you = members
        .iter()
        .find(|m| m.plan_task_id == task.plan_task_id)
        .cloned()
        .unwrap_or(TeamMember {
            plan_task_id: task.plan_task_id.clone(),
            title: task.title.clone(),
            worker_profile: task.worker_profile.clone(),
            status: task.status.clone(),
            depends_on: vec![],
            result_excerpt: None,
        });
    write_team_md(work_dir, &task.plan_task_id, &members).await;
    let run_tasks: Vec<&crate::store::BoardTaskRow> = snap
        .tasks
        .iter()
        .filter(|t| t.run_id == task.run_id)
        .collect();
    let shared = crate::orch::session_context::list_shared_files(
        &crate::sandbox::share::run_workspace_dir(data_dir, task.project_id, task.run_id),
    );
    let _ = crate::orch::session_context::write_run_context(
        work_dir,
        &task.plan_task_id,
        &run_tasks,
        &shared,
    );
    copy_dep_handoffs(work_dir, &you, data_dir, task).await;
    let talk = crate::sandbox::team_chat::TeamTalk::from_spawn(
        data_dir,
        task.project_id,
        task.run_id,
        &task.plan_task_id,
        &task.worker_profile,
    );
    crate::sandbox::team_chat::write_team_talk(work_dir, &talk).await;
    crate::sandbox::team_chat::copy_team_chat_into(work_dir, &talk).await;
    crate::sandbox::share::hydrate_session_files(data_dir, work_dir, task, snap, &you.depends_on)
        .await;
}

/// Refresh TEAM.md in every live session + archive result.md before wipe.
pub async fn sync_team_boards(data_dir: &Path, project_id: uuid::Uuid, snap: &BoardSnapshot) {
    for t in &snap.tasks {
        if t.project_id != project_id {
            continue;
        }
        if t.status == "done_success" {
            if let Some(s) = snap
                .sessions
                .iter()
                .filter(|s| s.board_task_id == t.id)
                .max_by_key(|s| s.created_at)
            {
                let dir = session_dir(data_dir, project_id, s.id);
                archive_session_result(data_dir, project_id, t.run_id, &t.plan_task_id, &dir).await;
            }
        }
    }

    let live: Vec<_> = snap
        .sessions
        .iter()
        .filter(|s| {
            s.project_id == project_id
                && matches!(
                    s.status.as_str(),
                    "spawning" | "healthy" | "stuck" | "killing"
                )
        })
        .collect();

    for s in live {
        let Some(task) = snap.tasks.iter().find(|t| t.id == s.board_task_id) else {
            continue;
        };
        let dir = session_dir(data_dir, project_id, s.id);
        if !dir.exists() {
            continue;
        }
        let members = members_from_snapshot(snap, task.run_id, data_dir);
        write_team_md(&dir, &task.plan_task_id, &members).await;
        let run_tasks: Vec<&crate::store::BoardTaskRow> = snap
            .tasks
            .iter()
            .filter(|t| t.run_id == task.run_id)
            .collect();
        let shared = crate::orch::list_shared_files(&crate::sandbox::share::run_workspace_dir(
            data_dir,
            project_id,
            task.run_id,
        ));
        let _ = crate::orch::write_run_context(&dir, &task.plan_task_id, &run_tasks, &shared);
        if let Some(you) = members.iter().find(|m| m.plan_task_id == task.plan_task_id) {
            copy_dep_handoffs(&dir, you, data_dir, task).await;
        }
        let talk = crate::sandbox::team_chat::TeamTalk::from_spawn(
            data_dir,
            project_id,
            task.run_id,
            &task.plan_task_id,
            &task.worker_profile,
        );
        crate::sandbox::team_chat::write_team_talk(&dir, &talk).await;
        crate::sandbox::team_chat::copy_team_chat_into(&dir, &talk).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_member_board() -> Vec<TeamMember> {
        vec![
            TeamMember {
                plan_task_id: "t1".into(),
                title: "API".into(),
                worker_profile: "engineering-backend-architect".into(),
                status: "done_success".into(),
                depends_on: vec![],
                result_excerpt: Some("Added PATCH /settings".into()),
            },
            TeamMember {
                plan_task_id: "t2".into(),
                title: "Settings UI".into(),
                worker_profile: "frontend".into(),
                status: "in_progress".into(),
                depends_on: vec!["t1".into()],
                result_excerpt: None,
            },
            TeamMember {
                plan_task_id: "t3".into(),
                title: "Review".into(),
                worker_profile: "reviewer".into(),
                status: "queued".into(),
                depends_on: vec!["t1".into(), "t2".into()],
                result_excerpt: None,
            },
        ]
    }

    #[test]
    fn team_md_marks_you_and_lists_others() {
        let members = three_member_board();
        let md = render_team_md("t2", &members);
        assert!(
            md.contains("| `t2` | `frontend` | Settings UI | in progress ← you |"),
            "you-row must pin t2 frontend Settings UI in progress: {md}"
        );
        assert!(
            md.contains("| `t3` | `reviewer` | Review | queued |"),
            "teammate table must list t3 reviewer: {md}"
        );
        assert!(
            md.contains("You wait on / use output from: `t1`"),
            "t2 depends-on must emit wait-on t1: {md}"
        );
        assert!(
            md.contains("### `t1` — API\n\nAdded PATCH /settings"),
            "finished teammate t1 excerpt must appear under its heading: {md}"
        );
    }

    #[test]
    fn team_md_unknown_you_falls_back_without_you_mark() {
        let members = three_member_board();
        let md = render_team_md("t99", &members);
        assert!(
            md.contains("You are task `t99`."),
            "unknown you must use fallback header: {md}"
        );
        assert!(
            !md.contains("← you"),
            "unknown you must not mark any table row: {md}"
        );
    }

    #[test]
    fn team_md_omits_your_own_excerpt_from_handoffs() {
        let members = three_member_board();
        let md = render_team_md("t1", &members);
        assert!(
            !md.contains("## Handoffs from finished teammates"),
            "own excerpt must not open the handoffs section: {md}"
        );
        assert!(
            !md.contains("### `t1`"),
            "own excerpt must not appear under a t1 heading: {md}"
        );
    }

    #[test]
    fn team_md_table_one_line_escapes_pipe_newline_and_caps_80() {
        let title = format!("A|B\n{}", "x".repeat(90));
        let members = vec![TeamMember {
            plan_task_id: "t1".into(),
            title,
            worker_profile: "frontend".into(),
            status: "in_progress".into(),
            depends_on: vec![],
            result_excerpt: None,
        }];
        let md = render_team_md("t1", &members);
        let doing = format!("A/B {}", "x".repeat(76));
        assert_eq!(
            doing.chars().count(),
            80,
            "expected doing cell is 80 chars after |→/ take-80 newline→space"
        );
        assert!(
            md.contains(&format!(
                "| `t1` | `frontend` | {doing} | in progress ← you |"
            )),
            "table doing cell must slash-pipe, flatten newline, cap 80: {md}"
        );
    }

    fn board_task(
        project: uuid::Uuid,
        run: uuid::Uuid,
        plan_task_id: &str,
        title: &str,
        status: &str,
        worker_profile: &str,
    ) -> BoardTaskRow {
        let now = chrono::Utc::now();
        BoardTaskRow {
            id: uuid::Uuid::now_v7(),
            project_id: project,
            run_id: run,
            plan_id: uuid::Uuid::now_v7(),
            plan_task_id: plan_task_id.into(),
            title: title.into(),
            summary: String::new(),
            spec_md: String::new(),
            status: status.into(),
            depends_on: vec![],
            priority: 1,
            worker_profile: worker_profile.into(),
            acceptance: serde_json::json!([]),
            external_ref: None,
            timeout_s: None,
            max_retries: 0,
            attempt: 0,
            claimed_session_id: None,
            last_report_seq: 0,
            progress_pct: 0,
            block_reason: None,
            hitl_notes: None,
            hitl_artifacts: None,
            hitl_resume_requested_at: None,
            hitl_notified_at: None,
            hitl_resolved_at: None,
            failure_reason: None,
            artifacts: None,
            retry_not_before: None,
            assigned_at: None,
            started_at: None,
            terminal_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn write_durable_handoff(
        data_dir: &std::path::Path,
        project: uuid::Uuid,
        run: uuid::Uuid,
        plan_task_id: &str,
        body: &str,
    ) {
        let dir = run_handoffs_dir(data_dir, project, run);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{plan_task_id}.md")), body).unwrap();
    }

    fn snapshot_member<'a>(members: &'a [TeamMember], plan_task_id: &str) -> &'a TeamMember {
        members
            .iter()
            .find(|m| m.plan_task_id == plan_task_id)
            .unwrap_or_else(|| panic!("missing member {plan_task_id}"))
    }

    #[test]
    fn excerpt_truncates_long_handoffs() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let prefix = "H".repeat(4000);
        let tail = "T".repeat(50);
        let long = format!("{prefix}{tail}");
        write_durable_handoff(root.path(), project, run, "t1", &long);
        write_durable_handoff(root.path(), project, run, "t3", "implemented API");
        let t1 = board_task(project, run, "t1", "API", "done_success", "coding");
        let t2 = board_task(project, run, "t2", "UI", "in_progress", "frontend");
        let t3 = board_task(project, run, "t3", "Docs", "done_success", "coding");
        let snap = BoardSnapshot {
            tasks: vec![t1, t2, t3],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        let expected = format!("{prefix}\n…(truncated)");
        assert_eq!(
            snapshot_member(&members, "t1").result_excerpt.as_deref(),
            Some(expected.as_str()),
            "done_success durable must keep the first 4000 chars and append the marker"
        );
        assert_eq!(
            snapshot_member(&members, "t3").result_excerpt.as_deref(),
            Some("implemented API"),
            "short durable must stay verbatim"
        );
        let md = render_team_md("t2", &members);
        assert!(
            md.contains(&format!("### `t1` — API\n\n{expected}")),
            "TEAM.md must dump the capped t1 prefix+marker: {md}"
        );
        assert!(
            !md.contains(&tail),
            "TEAM.md must not include the dropped tail: {md}"
        );
        assert!(
            md.contains("### `t3` — Docs\n\nimplemented API"),
            "TEAM.md must dump the short t3 excerpt verbatim: {md}"
        );
    }

    #[test]
    fn members_from_snapshot_trims_short_handoff() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        write_durable_handoff(root.path(), project, run, "t1", "  short  \n");
        let t1 = board_task(project, run, "t1", "API", "done_success", "coding");
        let snap = BoardSnapshot {
            tasks: vec![t1],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        assert_eq!(
            snapshot_member(&members, "t1").result_excerpt.as_deref(),
            Some("short"),
            "short padded durable must trim and stay verbatim"
        );
    }

    #[test]
    fn members_from_snapshot_falls_back_to_latest_session_result_when_durable_empty() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let t1 = board_task(project, run, "t1", "API", "done_success", "coding");
        write_durable_handoff(root.path(), project, run, "t1", "  \n");
        let now = chrono::Utc::now();
        let older = crate::store::WorkerSessionRow {
            id: uuid::Uuid::now_v7(),
            project_id: project,
            run_id: run,
            board_task_id: t1.id,
            attempt: 1,
            status: "exited".into(),
            runtime_backend: "process".into(),
            runtime_handle: None,
            worker_token_hash: String::new(),
            worker_profile: "coding".into(),
            last_heartbeat_at: None,
            progress_pct: 0,
            exit_reason: None,
            kill_grace_deadline: None,
            work_dir: None,
            started_at: None,
            exited_at: None,
            created_at: now - chrono::Duration::seconds(60),
        };
        let newer = crate::store::WorkerSessionRow {
            id: uuid::Uuid::now_v7(),
            project_id: project,
            run_id: run,
            board_task_id: t1.id,
            attempt: 2,
            status: "exited".into(),
            runtime_backend: "process".into(),
            runtime_handle: None,
            worker_token_hash: String::new(),
            worker_profile: "coding".into(),
            last_heartbeat_at: None,
            progress_pct: 0,
            exit_reason: None,
            kill_grace_deadline: None,
            work_dir: None,
            started_at: None,
            exited_at: None,
            created_at: now,
        };
        let old_prefix = "OLD".to_string() + &"o".repeat(3997);
        let new_prefix = "NEW".to_string() + &"n".repeat(3997);
        let old_body = format!("{}{}", old_prefix, "O".repeat(50));
        let new_body = format!("{}{}", new_prefix, "N".repeat(50));
        for (session_id, body) in [(older.id, old_body.as_str()), (newer.id, new_body.as_str())] {
            let dir = session_dir(root.path(), project, session_id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("result.md"), body).unwrap();
        }
        let snap = BoardSnapshot {
            tasks: vec![t1],
            sessions: vec![older, newer],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        let expected = format!("{new_prefix}\n…(truncated)");
        assert_eq!(
            snapshot_member(&members, "t1").result_excerpt.as_deref(),
            Some(expected.as_str()),
            "whitespace-only durable must fall back to the newest session prefix"
        );
    }

    #[test]
    fn members_from_snapshot_omits_excerpt_unless_done_success() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let huge = format!("{}{}", "H".repeat(4000), "T".repeat(50));
        write_durable_handoff(root.path(), project, run, "t1", &huge);
        write_durable_handoff(root.path(), project, run, "t2", &huge);
        let t1 = board_task(project, run, "t1", "API", "in_progress", "coding");
        let t2 = board_task(project, run, "t2", "UI", "done_failed", "frontend");
        let t3 = board_task(project, run, "t3", "Review", "ready", "reviewer");
        let snap = BoardSnapshot {
            tasks: vec![t1, t2, t3],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        assert_eq!(
            snapshot_member(&members, "t1").result_excerpt,
            None,
            "in_progress must not excerpt a huge durable handoff"
        );
        assert_eq!(
            snapshot_member(&members, "t2").result_excerpt,
            None,
            "done_failed must not excerpt a huge durable handoff"
        );
        let md = render_team_md("t3", &members);
        assert!(
            !md.contains("## Handoffs from finished teammates"),
            "non-success teammates must not open the Handoffs section: {md}"
        );
        assert!(
            !md.contains(&huge[..32]),
            "TEAM.md must not dump a non-success handoff body: {md}"
        );
    }

    #[tokio::test]
    async fn copy_dep_handoffs_keeps_full_body_when_team_excerpt_is_capped() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let prefix = "H".repeat(4000);
        let tail = "T".repeat(50);
        let long = format!("{prefix}{tail}");
        write_durable_handoff(root.path(), project, run, "t1", &long);
        let t1 = board_task(project, run, "t1", "API", "done_success", "coding");
        let mut t2 = board_task(project, run, "t2", "UI", "ready", "frontend");
        t2.depends_on = vec![t1.id];
        let snap = BoardSnapshot {
            tasks: vec![t1, t2.clone()],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        let expected = format!("{prefix}\n…(truncated)");
        assert_eq!(
            snapshot_member(&members, "t1").result_excerpt.as_deref(),
            Some(expected.as_str()),
            "TEAM excerpt must be the capped prefix, not the full durable file"
        );
        let you = snapshot_member(&members, "t2");
        assert_eq!(
            you.depends_on,
            vec!["t1".to_string()],
            "t2 must depend on t1 so copy_dep_handoffs plants handoff/t1.md"
        );
        let dest = root.path().join("next");
        copy_dep_handoffs(&dest, you, root.path(), &t2).await;
        assert_eq!(
            tokio::fs::read_to_string(dest.join("handoff/t1.md"))
                .await
                .unwrap(),
            long,
            "work_dir/handoff/t1.md must keep the full durable body"
        );
    }

    fn ready_member(plan_task_id: &str, depends_on: &[&str]) -> TeamMember {
        TeamMember {
            plan_task_id: plan_task_id.into(),
            title: "UI".into(),
            worker_profile: "frontend".into(),
            status: "ready".into(),
            depends_on: depends_on.iter().map(|s| (*s).to_string()).collect(),
            result_excerpt: None,
        }
    }

    #[tokio::test]
    async fn archive_and_copy_dep_handoffs() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let session = root.path().join("session");
        std::fs::create_dir_all(&session).unwrap();
        tokio::fs::write(session.join("result.md"), "implemented API\n")
            .await
            .unwrap();

        archive_session_result(root.path(), project, run, "t1", &session).await;
        let durable = run_handoffs_dir(root.path(), project, run).join("t1.md");
        assert_eq!(
            tokio::fs::read_to_string(&durable).await.unwrap(),
            "implemented API\n",
            "non-empty session result.md must land at runs/.../handoffs/t1.md"
        );
    }

    #[tokio::test]
    async fn archive_session_result_skips_missing_or_blank_result_md() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let missing = root.path().join("missing");
        std::fs::create_dir_all(&missing).unwrap();
        archive_session_result(root.path(), project, run, "t1", &missing).await;

        let blank = root.path().join("blank");
        std::fs::create_dir_all(&blank).unwrap();
        tokio::fs::write(blank.join("result.md"), "  \n")
            .await
            .unwrap();
        archive_session_result(root.path(), project, run, "t2", &blank).await;

        let durable = run_handoffs_dir(root.path(), project, run);
        assert!(
            !durable.join("t1.md").exists(),
            "missing result.md must not write durable t1.md"
        );
        assert!(
            !durable.join("t2.md").exists(),
            "whitespace-only result.md must not write durable t2.md"
        );
    }

    #[tokio::test]
    async fn copy_dep_handoffs_copies_only_listed_deps() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        write_durable_handoff(root.path(), project, run, "t1", "from t1\n");
        write_durable_handoff(root.path(), project, run, "t2", "from t2\n");
        let task = board_task(project, run, "t3", "UI", "ready", "frontend");

        let dest = root.path().join("next");
        copy_dep_handoffs(&dest, &ready_member("t3", &["t1"]), root.path(), &task).await;
        assert_eq!(
            tokio::fs::read_to_string(dest.join("handoff/t1.md"))
                .await
                .unwrap(),
            "from t1\n",
            "you.depends_on=[t1] must copy durable t1.md into work_dir/handoff/"
        );
        assert!(
            !dest.join("handoff/t2.md").exists(),
            "unlisted durable t2.md must not be copied"
        );

        let empty = root.path().join("empty");
        copy_dep_handoffs(&empty, &ready_member("t3", &[]), root.path(), &task).await;
        assert!(
            !empty.join("handoff").exists(),
            "empty you.depends_on must not create work_dir/handoff/"
        );
    }

    #[tokio::test]
    async fn durable_handoff_survives_session_wipe() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let session = root.path().join("session");
        std::fs::create_dir_all(&session).unwrap();
        tokio::fs::write(session.join("result.md"), "implemented API\n")
            .await
            .unwrap();

        archive_session_result(root.path(), project, run, "t1", &session).await;
        tokio::fs::remove_dir_all(&session).await.unwrap();

        let dest = root.path().join("next");
        let task = board_task(project, run, "t2", "UI", "ready", "frontend");
        copy_dep_handoffs(&dest, &ready_member("t2", &["t1"]), root.path(), &task).await;
        assert_eq!(
            tokio::fs::read_to_string(dest.join("handoff/t1.md"))
                .await
                .unwrap(),
            "implemented API\n",
            "copy_dep_handoffs must still plant dest/handoff/t1.md after session wipe"
        );
    }

    #[tokio::test]
    async fn prepare_workspace_writes_team_md() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let task = board_task(project, run, "t1", "API", "ready", "coding");
        let snap = BoardSnapshot {
            tasks: vec![task.clone()],
            sessions: vec![],
        };
        let work = root.path().join("work");
        tokio::fs::create_dir_all(&work).await.unwrap();
        prepare_agent_workspace(root.path(), &work, &task, &snap).await;
        let md = tokio::fs::read_to_string(work.join("TEAM.md"))
            .await
            .unwrap();
        assert!(
            md.contains("You are **`t1`**"),
            "spawn TEAM.md must pin you-header for t1: {md}"
        );
        let talk = crate::sandbox::team_chat::load_team_talk(&work).expect("talk file");
        assert_eq!(talk.me, "t1", "spawn .team.json me must be plan_task_id t1");
        assert_eq!(
            talk.run_id, run,
            "spawn .team.json run_id must match the board task"
        );
        let chat = tokio::fs::read_to_string(work.join("TEAM_CHAT.md"))
            .await
            .unwrap();
        assert!(
            chat.contains("_No messages yet._"),
            "empty spawn TEAM_CHAT.md must plant the empty-header: {chat}"
        );
    }

    #[test]
    fn members_from_snapshot_maps_uuid_depends_on_to_plan_task_ids() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let t1 = board_task(project, run, "t1", "API", "ready", "coding");
        let mut t2 = board_task(project, run, "t2", "UI", "ready", "frontend");
        t2.depends_on = vec![t1.id];
        let snap = BoardSnapshot {
            tasks: vec![t1, t2],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run, root.path());
        assert_eq!(
            snapshot_member(&members, "t2").depends_on,
            vec!["t1".to_string()],
            "t2.depends_on must map t1.id to plan id t1, not the UUID"
        );
    }

    #[tokio::test]
    async fn prepare_workspace_fallback_you_when_task_absent_from_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let task = board_task(project, run, "t1", "API", "ready", "coding");
        let snap = BoardSnapshot {
            tasks: vec![],
            sessions: vec![],
        };
        let work = root.path().join("work");
        tokio::fs::create_dir_all(&work).await.unwrap();
        prepare_agent_workspace(root.path(), &work, &task, &snap).await;
        let md = tokio::fs::read_to_string(work.join("TEAM.md"))
            .await
            .unwrap();
        assert!(
            md.contains("You are task `t1`."),
            "absent-from-snap must emit fallback you-header: {md}"
        );
        let talk = crate::sandbox::team_chat::load_team_talk(&work).expect("talk file");
        assert_eq!(talk.me, "t1", "fallback spawn still plants talk.me=t1");
    }

    #[test]
    fn members_from_snapshot_filters_tasks_to_this_run_id() {
        let root = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run_a = uuid::Uuid::now_v7();
        let run_b = uuid::Uuid::now_v7();
        let t1 = board_task(project, run_a, "t1", "API", "ready", "coding");
        let tx = board_task(project, run_b, "tX", "Other", "ready", "coding");
        let snap = BoardSnapshot {
            tasks: vec![t1, tx],
            sessions: vec![],
        };
        let members = members_from_snapshot(&snap, run_a, root.path());
        assert_eq!(
            members
                .iter()
                .map(|m| m.plan_task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["t1"],
            "members must keep only run A's t1, not run B's tX"
        );
    }
}
