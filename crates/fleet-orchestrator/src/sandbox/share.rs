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

    #[test]
    fn changes_request_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let req = ChangesRequest {
            targets: vec!["t1".into()],
            reason: "  missing tests\n".into(),
        };
        write_changes_request(dir.path(), &req).unwrap();
        let got = read_changes_request(dir.path()).unwrap();
        assert_eq!(got.targets, ["t1"]);
        assert_eq!(got.reason, "  missing tests\n");
        let review = std::fs::read_to_string(dir.path().join("REVIEW.md")).unwrap();
        assert_eq!(
            review,
            "# Changes requested\n\nTargets: t1\n\nmissing tests\n"
        );
    }

    #[test]
    fn write_changes_request_rejects_blank_reason() {
        for reason in ["", "  \n"] {
            let dir = tempfile::tempdir().unwrap();
            let err = write_changes_request(
                dir.path(),
                &ChangesRequest {
                    targets: vec!["t1".into()],
                    reason: reason.into(),
                },
            )
            .unwrap_err();
            assert_eq!(err, "reason is empty", "reason={reason:?}");
            assert!(
                !changes_path(dir.path()).exists(),
                "blank reason {reason:?} must not write request_changes.json"
            );
            assert!(
                !dir.path().join("REVIEW.md").exists(),
                "blank reason {reason:?} must not write REVIEW.md"
            );
        }
    }

    #[test]
    fn write_changes_request_empty_targets_labels_dependencies_in_review_md() {
        let dir = tempfile::tempdir().unwrap();
        write_changes_request(
            dir.path(),
            &ChangesRequest {
                targets: vec![],
                reason: "  need deps  ".into(),
            },
        )
        .unwrap();
        let review = std::fs::read_to_string(dir.path().join("REVIEW.md")).unwrap();
        assert_eq!(
            review,
            "# Changes requested\n\nTargets: (dependencies)\n\nneed deps\n"
        );
    }

    #[test]
    fn read_changes_request_none_when_missing_or_invalid() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_changes_request(dir.path()).is_none(),
            "absent request_changes.json must skip bounce"
        );

        let path = changes_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        assert!(
            read_changes_request(dir.path()).is_none(),
            "empty request_changes.json must skip bounce"
        );

        std::fs::write(&path, "{").unwrap();
        assert!(
            read_changes_request(dir.path()).is_none(),
            "invalid request_changes.json must skip bounce"
        );
    }
}
