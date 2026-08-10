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
            me.plan_task_id, me.worker_profile, me.title, human_status(&me.status)
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
        let mark = if m.plan_task_id == you { " ← you" } else { "" };
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
         - If someone is `in progress`, assume they own that slice — wait or use their handoff when they finish.\n\
         - If you depend on others, read `handoff/` before writing code.\n\
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
    tasks.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.created_at.cmp(&b.created_at)));

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
pub async fn copy_dep_handoffs(work_dir: &Path, you: &TeamMember, data_dir: &Path, task: &BoardTaskRow) {
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
    copy_dep_handoffs(work_dir, &you, data_dir, task).await;
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
        if let Some(you) = members.iter().find(|m| m.plan_task_id == task.plan_task_id) {
            copy_dep_handoffs(&dir, you, data_dir, task).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_md_marks_you_and_lists_others() {
        let members = vec![
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
        ];
        let md = render_team_md("t2", &members);
        assert!(md.contains("← you"));
        assert!(md.contains("Settings UI"));
        assert!(md.contains("Added PATCH /settings"));
        assert!(md.contains("reviewer"));
        assert!(md.contains("Do **only your** task"));
    }
}
