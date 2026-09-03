//! Shared run workspace so agents collaborate on the same files.
//!
//! Canonical tree: `DATA_DIR/projects/{project}/runs/{run}/workspace/`
//! Each session gets a copy at `work_dir/work/`. On success we sync back.
//! Finished trees are archived under `handoffs/{plan_task_id}/tree/`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{BoardSnapshot, BoardTaskRow};

const SEEDED: &str = ".seeded";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesRequest {
    #[serde(default)]
    pub targets: Vec<String>,
    pub reason: String,
}

pub fn run_workspace_dir(data_dir: &Path, project_id: Uuid, run_id: Uuid) -> PathBuf {
    super::team_chat::run_dir(data_dir, project_id, run_id).join("workspace")
}

pub fn changes_path(work_dir: &Path) -> PathBuf {
    work_dir.join(".fleet").join("request_changes.json")
}

pub fn skip_name(name: &str) -> bool {
    matches!(
        name,
        "PERSONA.md"
            | "TEAM.md"
            | "TEAM_CHAT.md"
            | "task.md"
            | "result.md"
            | "stdout.log"
            | "stderr.log"
            | ".team.json"
            | "pid"
            | "handoff"
            | ".fleet"
            | ".seeded"
            | ".lock"
            | ".seed.lock"
            | "commands.sh"
            | "fleet_worker.sh"
            | "target"
            | "node_modules"
            | ".git"
    ) || name.starts_with(".fleet")
}

/// Create the run workspace once. Agents share this folder — no git clone required.
pub async fn ensure_run_workspace(
    data_dir: &Path,
    project_id: Uuid,
    run_id: Uuid,
    git_url: Option<&str>,
    git_ref: Option<&str>,
) -> PathBuf {
    let dir = run_workspace_dir(data_dir, project_id, run_id);
    let _ = tokio::fs::create_dir_all(&dir).await;
    if dir.join(SEEDED).exists() {
        return dir;
    }
    if let Some(url) = git_url.map(str::trim).filter(|s| !s.is_empty()) {
        let project_dir = data_dir.join("projects").join(project_id.to_string());
        let seed = super::team_chat::run_dir(data_dir, project_id, run_id).join(".gitseed");
        let _ = tokio::fs::create_dir_all(&seed).await;
        match super::git_isolate::attach_worktree(&project_dir, &seed, url).await {
            Ok(repo) => {
                if let Some(rf) = git_ref.map(str::trim).filter(|s| !s.is_empty()) {
                    let _ = tokio::process::Command::new("git")
                        .args(["-C", &repo.to_string_lossy(), "checkout", "--quiet", rf])
                        .output()
                        .await;
                }
                copy_dir_filtered(&repo, &dir);
            }
            Err(e) => {
                tracing::warn!(error = %e, "shared workspace git seed skipped");
            }
        }
        let _ = tokio::fs::remove_dir_all(&seed).await;
    }
    if dir_is_empty(&dir) {
        let _ = std::fs::write(
            dir.join("README.md"),
            "# Shared workspace\n\nAll agents on this run edit `work/`. This is the team's artifacts, not a git clone.\n",
        );
    }
    let _ = std::fs::write(dir.join(SEEDED), "ok\n");
    dir
}

pub fn materialize_session_repo(data_dir: &Path, work_dir: &Path, task: &BoardTaskRow) {
    let ws = run_workspace_dir(data_dir, task.project_id, task.run_id);
    let dest = work_dir.join("work");
    let _ = std::fs::create_dir_all(&dest);
    if ws.exists() {
        copy_dir_filtered(&ws, &dest);
    }
}

pub fn sync_session_repo_back(data_dir: &Path, work_dir: &Path, task: &BoardTaskRow) {
    let src = work_dir.join("work");
    if !src.is_dir() {
        return;
    }
    let dest = run_workspace_dir(data_dir, task.project_id, task.run_id);
    let _ = std::fs::create_dir_all(&dest);
    copy_dir_filtered(&src, &dest);
}

pub fn archive_session_tree(
    data_dir: &Path,
    project_id: Uuid,
    run_id: Uuid,
    plan_task_id: &str,
    work_dir: &Path,
) {
    let src = work_dir.join("work");
    if !src.is_dir() {
        return;
    }
    let dest = super::team::run_handoffs_dir(data_dir, project_id, run_id)
        .join(plan_task_id)
        .join("tree");
    let _ = std::fs::create_dir_all(&dest);
    copy_dir_filtered(&src, &dest);
}

pub async fn copy_dep_trees(
    work_dir: &Path,
    you_deps: &[String],
    data_dir: &Path,
    task: &BoardTaskRow,
) {
    if you_deps.is_empty() {
        return;
    }
    let src_root = super::team::run_handoffs_dir(data_dir, task.project_id, task.run_id);
    for id in you_deps {
        let src = src_root.join(id).join("tree");
        if !src.is_dir() {
            continue;
        }
        let dest = work_dir.join("handoff").join(id);
        let _ = tokio::fs::create_dir_all(&dest).await;
        copy_dir_filtered(&src, &dest);
    }
}

pub fn write_changes_request(work_dir: &Path, req: &ChangesRequest) -> Result<(), String> {
    if req.reason.trim().is_empty() {
        return Err("reason is empty".into());
    }
    let dir = work_dir.join(".fleet");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(req).map_err(|e| e.to_string())?;
    std::fs::write(changes_path(work_dir), body).map_err(|e| e.to_string())?;
    let _ = std::fs::write(
        work_dir.join("REVIEW.md"),
        format!(
            "# Changes requested\n\nTargets: {}\n\n{}\n",
            if req.targets.is_empty() {
                "(dependencies)".into()
            } else {
                req.targets.join(", ")
            },
            req.reason.trim()
        ),
    );
    Ok(())
}

pub fn read_changes_request(work_dir: &Path) -> Option<ChangesRequest> {
    let raw = std::fs::read_to_string(changes_path(work_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn copy_dir_filtered(src: &Path, dest: &Path) {
    let _ = std::fs::create_dir_all(dest);
    let Ok(rd) = std::fs::read_dir(src) else {
        return;
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let Some(n) = name.to_str() else { continue };
        if skip_name(n) {
            continue;
        }
        let from = ent.path();
        let to = dest.join(&name);
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir_filtered(&from, &to);
        } else if let Ok(bytes) = std::fs::read(&from) {
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&to, bytes);
        }
    }
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| {
            it.all(|e| {
                e.ok()
                    .and_then(|e| e.file_name().into_string().ok())
                    .map(|n| skip_name(&n) || n == SEEDED)
                    .unwrap_or(true)
            })
        })
        .unwrap_or(true)
}

/// Copy finished teammate trees + shared workspace into a live session.
pub async fn hydrate_session_files(
    data_dir: &Path,
    work_dir: &Path,
    task: &BoardTaskRow,
    snap: &BoardSnapshot,
    deps: &[String],
) {
    ensure_run_workspace(data_dir, task.project_id, task.run_id, None, None).await;
    materialize_session_repo(data_dir, work_dir, task);
    copy_dep_trees(work_dir, deps, data_dir, task).await;
    let _ = snap;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_skips_control_files_and_keeps_code() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        let dest = root.path().join("dest");
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("TEAM.md"), "no").unwrap();
        std::fs::write(src.join("README.md"), "hi").unwrap();
        copy_dir_filtered(&src, &dest);
        assert!(dest.join("src/main.rs").exists());
        assert!(dest.join("README.md").exists());
        assert!(!dest.join("TEAM.md").exists());
    }

    /// Docs §16: Settings git remote cleared → new tasks skip clone
    /// (prompt-only). A leftover per-project `source.git` from the previous
    /// remote must not seed a later run.
    #[tokio::test]
    async fn new_spawn_skips_clone_when_remote_cleared() {
        let root = tempfile::tempdir().unwrap();
        let old_src = root.path().join("remote-old");
        std::fs::create_dir_all(&old_src).unwrap();
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok();
        assert!(
            git_ok,
            "git is required for share skip-clone tests (install git; see install-full-suite-deps.sh)"
        );
        assert!(
            std::process::Command::new("git")
                .args(["-C", old_src.to_str().unwrap(), "init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        let _ = std::process::Command::new("git")
            .current_dir(&old_src)
            .args(["config", "user.email", "t@t.t"])
            .status();
        let _ = std::process::Command::new("git")
            .current_dir(&old_src)
            .args(["config", "user.name", "t"])
            .status();
        std::fs::write(old_src.join("FROM_OLD.txt"), "alpha-remote").unwrap();
        std::fs::write(old_src.join("README.md"), "old-remote").unwrap();
        assert!(
            std::process::Command::new("git")
                .current_dir(&old_src)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .current_dir(&old_src)
                .args(["commit", "-qm", "init"])
                .status()
                .unwrap()
                .success()
        );

        let project = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let first_run = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let old_url = old_src.to_str().unwrap();
        let first =
            ensure_run_workspace(root.path(), project, first_run, Some(old_url), Some("HEAD"))
                .await;
        assert_eq!(
            std::fs::read_to_string(first.join("FROM_OLD.txt")).unwrap_or_default(),
            "alpha-remote",
            "control seed must clone the Settings remote before it is cleared"
        );
        assert!(
            root.path()
                .join("projects")
                .join(project.to_string())
                .join("source.git")
                .join("HEAD")
                .exists(),
            "previous remote must leave a cache the cleared-remote spawn must not reuse"
        );

        let cleared_run = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let cleared = ensure_run_workspace(root.path(), project, cleared_run, None, None).await;
        assert!(
            !cleared.join("FROM_OLD.txt").exists(),
            "new run after Settings clear must not clone the previous remote (leftover source.git)"
        );
        let readme = std::fs::read_to_string(cleared.join("README.md")).unwrap_or_default();
        assert!(
            readme.contains("Shared workspace") && !readme.contains("old-remote"),
            "cleared remote must seed a prompt-only shared workspace, not the previous checkout: {readme:?}"
        );
        assert!(
            !readme.contains("alpha-remote"),
            "prompt-only workspace must not contain leftover remote contents"
        );

        let whitespace_run = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let whitespace = ensure_run_workspace(
            root.path(),
            project,
            whitespace_run,
            Some("   "),
            Some(old_url),
        )
        .await;
        assert!(
            !whitespace.join("FROM_OLD.txt").exists(),
            "whitespace Settings URL is a clear and must not clone leftover remote"
        );
        let empty_run = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let empty =
            ensure_run_workspace(root.path(), project, empty_run, Some(""), Some("main")).await;
        assert!(
            !empty.join("FROM_OLD.txt").exists(),
            "empty Settings URL is a clear and must not clone leftover remote"
        );

        assert_eq!(
            std::fs::read_to_string(first.join("FROM_OLD.txt")).unwrap(),
            "alpha-remote",
            "skipping clone on a later run must not rewrite the already-seeded workspace"
        );

        let still_set_run = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
        let still = ensure_run_workspace(
            root.path(),
            project,
            still_set_run,
            Some(old_url),
            Some("HEAD"),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(still.join("FROM_OLD.txt")).unwrap_or_default(),
            "alpha-remote",
            "control: a still-set Settings remote must still clone (not a tautology that every seed skips)"
        );
        assert!(
            !cleared.join("FROM_OLD.txt").exists(),
            "control clone must not backfill the skipped cleared-remote workspace"
        );
    }

    #[tokio::test]
    async fn seed_empty_workspace_and_roundtrip_repo() {
        let root = tempfile::tempdir().unwrap();
        let project = Uuid::now_v7();
        let run = Uuid::now_v7();
        let ws = ensure_run_workspace(root.path(), project, run, None, None).await;
        assert!(ws.join("README.md").exists());
        assert!(ws.join(SEEDED).exists());
        // second call is no-op
        let _ = std::fs::write(ws.join("api.rs"), "ok");
        ensure_run_workspace(root.path(), project, run, None, None).await;
        assert_eq!(std::fs::read_to_string(ws.join("api.rs")).unwrap(), "ok");

        let task = dummy_task(project, run, "t1");
        let session = root.path().join("sess");
        std::fs::create_dir_all(&session).unwrap();
        materialize_session_repo(root.path(), &session, &task);
        assert!(session.join("work/api.rs").exists());
        std::fs::write(session.join("work/api.rs"), "v2").unwrap();
        std::fs::write(session.join("work/ui.tsx"), "x").unwrap();
        sync_session_repo_back(root.path(), &session, &task);
        archive_session_tree(root.path(), project, run, "t1", &session);
        assert_eq!(std::fs::read_to_string(ws.join("api.rs")).unwrap(), "v2");
        assert!(ws.join("ui.tsx").exists());
        let archived = crate::sandbox::team::run_handoffs_dir(root.path(), project, run)
            .join("t1/tree/ui.tsx");
        assert!(archived.exists());
    }

    #[test]
    fn changes_request_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let req = ChangesRequest {
            targets: vec!["t1".into()],
            reason: "missing tests".into(),
        };
        write_changes_request(dir.path(), &req).unwrap();
        let got = read_changes_request(dir.path()).unwrap();
        assert_eq!(got.targets, ["t1"]);
        assert!(dir.path().join("REVIEW.md").exists());
    }

    fn dummy_task(project: Uuid, run: Uuid, id: &str) -> BoardTaskRow {
        BoardTaskRow {
            id: Uuid::now_v7(),
            project_id: project,
            run_id: run,
            plan_id: Uuid::now_v7(),
            plan_task_id: id.into(),
            title: id.into(),
            summary: String::new(),
            spec_md: String::new(),
            status: "ready".into(),
            depends_on: vec![],
            priority: 1,
            worker_profile: "coding".into(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}
