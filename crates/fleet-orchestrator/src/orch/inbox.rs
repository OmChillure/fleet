//! Step 1 — apply worker_reports inbox.

use crate::sandbox::{RuntimeHandle, SandboxRuntime};
use crate::store::Store;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use super::tick::TickError;

pub async fn apply_inbox(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    project_id: Uuid,
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
