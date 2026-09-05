//! Step 1 — apply worker_reports inbox.

use crate::store::Store;
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

use super::can_apply_task_terminal;
use super::tick::TickError;

pub struct InboxApply {
    pub applied: usize,
    pub reviews_bounced: usize,
}

pub async fn apply_inbox(
    store: &Store,
    project_id: Uuid,
    data_dir: &Path,
) -> Result<InboxApply, TickError> {
    let reports = store.list_unapplied_reports(project_id).await?;
    let mut applied = 0usize;
    let mut reviews_bounced = 0usize;
    for r in &reports {
        match r.kind.as_str() {
            "heartbeat" => {
                let pct = r
                    .payload
                    .get("progress_pct")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i32);
                store
                    .apply_heartbeat(r.session_id, r.board_task_id, r.seq, pct)
                    .await?;
            }
            "status" => {
                let status = r
                    .payload
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match status {
                    "running" => {
                        // redundant with spawn success; optional heartbeat bump
                        store
                            .apply_heartbeat(r.session_id, r.board_task_id, r.seq, None)
                            .await?;
                    }
                    "success" => {
                        if let Some(session) = store.get_session(r.session_id).await? {
                            if let Some(task) = store
                                .load_board_snapshot(project_id)
                                .await?
                                .tasks
                                .iter()
                                .find(|t| t.id == r.board_task_id)
                                .cloned()
                            {
                                if task.claimed_session_id != Some(r.session_id) {
                                    store.mark_report_applied(r.id).await?;
                                    applied += 1;
                                    continue;
                                }
                                let work = session
                                    .work_dir
                                    .as_deref()
                                    .filter(|s| !s.is_empty())
                                    .map(std::path::PathBuf::from)
                                    .unwrap_or_else(|| {
                                        data_dir
                                            .join("projects")
                                            .join(project_id.to_string())
                                            .join("sessions")
                                            .join(session.id.to_string())
                                    });
                                let payload =
                                    merge_session_proof(&r.payload, &reports, r.session_id);
                                if let Err(e) = materialize_guest_proof(&work, &payload) {
                                    tracing::warn!(
                                        error = %e,
                                        session_id = %session.id,
                                        "failed to materialize guest proof onto plane work dir"
                                    );
                                }
                                let check = super::validate_done::validate_task_done(
                                    &task,
                                    &work,
                                    &session.runtime_backend,
                                );
                                if !check.ok {
                                    let bounces = task
                                        .hitl_notes
                                        .as_deref()
                                        .unwrap_or("")
                                        .matches("Task validation failed")
                                        .count();
                                    if bounces >= 2 {
                                        if !can_apply_task_terminal(
                                            &task,
                                            r.session_id,
                                            "done_failed",
                                        ) {
                                            store.mark_report_applied(r.id).await?;
                                            applied += 1;
                                            continue;
                                        }
                                        store
                                            .set_task_done_failed(
                                                r.board_task_id,
                                                "validation_failed_exhausted",
                                            )
                                            .await?;
                                    } else {
                                        if !can_apply_task_terminal(
                                            &task,
                                            r.session_id,
                                            "done_success",
                                        ) {
                                            store.mark_report_applied(r.id).await?;
                                            applied += 1;
                                            continue;
                                        }
                                        store
                                            .reopen_task_for_changes(
                                                r.board_task_id,
                                                "validation_failed",
                                                &check.notes(),
                                            )
                                            .await?;
                                    }
                                    store.mark_report_applied(r.id).await?;
                                    applied += 1;
                                    continue;
                                }
                                if !can_apply_task_terminal(
                                    &task,
                                    r.session_id,
                                    "done_success",
                                ) {
                                    store.mark_report_applied(r.id).await?;
                                    applied += 1;
                                    continue;
                                }
                                if let Some(req) =
                                    crate::sandbox::share::read_changes_request(&work)
                                {
                                    let snap = store.load_board_snapshot(project_id).await?;
                                    reviews_bounced += super::review::apply_review_bounce(
                                        store,
                                        &snap,
                                        r.board_task_id,
                                        &req,
                                        data_dir,
                                    )
                                    .await?;
                                    store.mark_report_applied(r.id).await?;
                                    applied += 1;
                                    continue;
                                }
                                store
                                    .apply_task_terminal(
                                        r.board_task_id,
                                        r.session_id,
                                        "done_success",
                                        None,
                                        r.seq,
                                    )
                                    .await?;
                            }
                        }
                    }
                    "proof" => {
                        if let Some(session) = store.get_session(r.session_id).await? {
                            let work = session
                                .work_dir
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .map(std::path::PathBuf::from)
                                .unwrap_or_else(|| {
                                    data_dir
                                        .join("projects")
                                        .join(project_id.to_string())
                                        .join("sessions")
                                        .join(session.id.to_string())
                                });
                            let _ = materialize_guest_proof(&work, &r.payload);
                        }
                    }
                    "failed" => {
                        let reason = r
                            .payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("worker_failed");
                        let snap = store.load_board_snapshot(project_id).await?;
                        if let Some(task) = snap.tasks.iter().find(|t| t.id == r.board_task_id) {
                            if !can_apply_task_terminal(task, r.session_id, "done_failed")
                            {
                                store.mark_report_applied(r.id).await?;
                                applied += 1;
                                continue;
                            }
                        } else {
                            store.mark_report_applied(r.id).await?;
                            applied += 1;
                            continue;
                        }
                        store
                            .apply_task_terminal(
                                r.board_task_id,
                                r.session_id,
                                "done_failed",
                                Some(reason),
                                r.seq,
                            )
                            .await?;
                    }
                    "blocked" => {
                        let reason = r
                            .payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("blocked");
                        let snap = store.load_board_snapshot(project_id).await?;
                        if let Some(task) = snap.tasks.iter().find(|t| t.id == r.board_task_id) {
                            if !can_apply_task_terminal(task, r.session_id, "blocked") {
                                store.mark_report_applied(r.id).await?;
                                applied += 1;
                                continue;
                            }
                        } else {
                            store.mark_report_applied(r.id).await?;
                            applied += 1;
                            continue;
                        }
                        store
                            .apply_task_terminal(
                                r.board_task_id,
                                r.session_id,
                                "blocked",
                                Some(reason),
                                r.seq,
                            )
                            .await?;
                    }
                    _ => {
                        // unknown status payload — still mark applied to avoid poison
                    }
                }
            }
            _ => {}
        }
        store.mark_report_applied(r.id).await?;
        applied += 1;
        let _ = Value::Null;
    }
    Ok(InboxApply {
        applied,
        reviews_bounced,
    })
}

/// Write `result.md` / `work/` from a guest status payload onto the plane
/// session dir so `validate_done` can see proof the guest actually produced.
pub fn materialize_guest_proof(work_dir: &Path, payload: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(work_dir)?;
    if let Some(md) = payload
        .get("result_md")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        std::fs::write(work_dir.join("result.md"), md)?;
    }
    if let Some(files) = payload.get("files").and_then(|v| v.as_object()) {
        for (rel, body) in files {
            let rel = rel.trim().trim_start_matches(['/', '\\']);
            if rel.is_empty() || rel.split(['/', '\\']).any(|p| p == ".." || p.is_empty()) {
                continue;
            }
            let Some(text) = body.as_str() else {
                continue;
            };
            let dest = work_dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(dest, text)?;
        }
    }
    Ok(())
}

fn merge_session_proof(
    base: &Value,
    reports: &[crate::store::WorkerReportRow],
    session_id: Uuid,
) -> Value {
    let mut out = base.clone();
    let missing_md = out
        .get("result_md")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .is_empty();
    if missing_md {
        for other in reports {
            if other.session_id != session_id {
                continue;
            }
            if let Some(md) = other
                .payload
                .get("result_md")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("result_md".into(), Value::String(md.to_string()));
                }
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn materialize_writes_result_md_without_inventing_work() {
        let dir = tempfile::tempdir().unwrap();
        materialize_guest_proof(
            dir.path(),
            &json!({
                "status": "success",
                "result_md": "# Result\n\nACP end_turn. hello from mock agent\n"
            }),
        )
        .unwrap();
        let md = std::fs::read_to_string(dir.path().join("result.md")).unwrap();
        assert!(md.contains("hello from mock agent"), "{md}");
        assert!(
            !dir.path().join("work").exists(),
            "result_md alone must not satisfy exists:work"
        );
    }

    #[test]
    fn materialize_rejects_parent_paths() {
        let dir = tempfile::tempdir().unwrap();
        materialize_guest_proof(
            dir.path(),
            &json!({
                "files": { "../outside": "nope", "work/ok.txt": "yes" }
            }),
        )
        .unwrap();
        assert!(!dir.path().parent().unwrap().join("outside").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/ok.txt")).unwrap(),
            "yes"
        );
    }

    #[test]
    fn materialize_without_proof_does_not_invent_result() {
        let dir = tempfile::tempdir().unwrap();
        materialize_guest_proof(dir.path(), &json!({ "status": "success" })).unwrap();
        assert!(!dir.path().join("result.md").exists());
    }
}
