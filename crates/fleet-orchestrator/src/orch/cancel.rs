//! Cancel drain for runs with cancel_requested_at.

use crate::store::{BoardSnapshot, Store};
use chrono::{Duration as ChronoDuration, Utc};
use uuid::Uuid;

use super::tick::TickError;

pub struct CancelOutcome {
    pub tasks_cancelled: usize,
    pub sessions_signaled: usize,
}

pub async fn cancel_drain(
    store: &Store,
    project_id: Uuid,
    snap: &BoardSnapshot,
    kill_grace_s: i32,
) -> Result<CancelOutcome, TickError> {
    let runs = store.list_cancelling_runs(project_id).await?;
    let mut out = CancelOutcome {
        tasks_cancelled: 0,
        sessions_signaled: 0,
    };
    let now = Utc::now();

    for run in runs {
        // Non-terminal tasks without a live claim → cancelled immediately
        let n = store
            .cancel_nonterminal_tasks_without_session(run.id)
            .await?;
        out.tasks_cancelled += n as usize;

        // Live sessions for this run → signal kill with cancel reason
        for session in snap.sessions.iter().filter(|s| s.run_id == run.id) {
            if matches!(session.status.as_str(), "exited" | "failed_spawn") {
                continue;
            }
            let grace = now + ChronoDuration::seconds(kill_grace_s as i64);
            store
                .mark_session_killing(session.id, "cancel", grace)
                .await?;
            let _ = store.void_spawn_claims(session.id).await;
            if session.runtime_backend == crate::sandbox::remote::BACKEND {
                if let Some(host_id) = crate::sandbox::remote::host_id_from_handle(
                    session.runtime_handle.as_deref().unwrap_or(""),
                ) {
                    let _ = crate::store::enqueue_signal_session(
                        store, project_id, session.id, host_id, "term",
                    )
                    .await;
                    out.sessions_signaled += 1;
                }
            } else {
                // spawning without handle
                store.complete_spawn_failure(session.id).await?;
                store
                    .set_task_cancelled(session.board_task_id, "cancel")
                    .await?;
                out.tasks_cancelled += 1;
            }
        }
    }
    Ok(out)
}
