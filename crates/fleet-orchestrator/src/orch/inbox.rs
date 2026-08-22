//! Step 1 — apply worker_reports inbox.

use crate::sandbox::{RuntimeHandle, SandboxRuntime};
use crate::store::Store;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

use super::tick::TickError;

pub async fn apply_inbox(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    project_id: Uuid,
    data_dir: &Path,
) -> Result<usize, TickError> {
    let reports = store.list_unapplied_reports(project_id).await?;
    let mut applied = 0usize;
    for r in reports {
        match r.kind.as_str() {
            "heartbeat" => {
                let pct = r.payload.get("progress_pct").and_then(|v| v.as_i64()).map(|n| n as i32);
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
                        destroy_session_runtime(store, runtime, r.session_id).await?;
                        if let Some(session) = store.get_session(r.session_id).await? {
                            if let Some(task) = store
                                .load_board_snapshot(project_id)
                                .await?
                                .tasks
                                .iter()
                                .find(|t| t.id == r.board_task_id)
                                .cloned()
                            {
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
                                        store
                                            .set_task_done_failed(
                                                r.board_task_id,
                                                "validation_failed_exhausted",
                                            )
                                            .await?;
                                    } else {
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
                            }
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
                    "failed" => {
                        let reason = r
                            .payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("worker_failed");
                        destroy_session_runtime(store, runtime, r.session_id).await?;
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
    Ok(applied)
}

pub async fn destroy_session_runtime(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    session_id: Uuid,
) -> Result<(), TickError> {
    if let Some(s) = store.get_session(session_id).await?
        && let Some(h) = s.runtime_handle {
            let handle = RuntimeHandle {
                backend: s.runtime_backend,
                id: h,
            };
            let _ = runtime.destroy(&handle).await;
        }
    Ok(())
}
