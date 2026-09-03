//! Step 8 — stuck detection, kill, retry for paired-host sessions.

use crate::domain::{ExitReason, WorkerSessionStatus};
use crate::store::{BoardSnapshot, Store};
use chrono::{Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::spawn::apply_retry_after_failure;
use super::tick::TickError;

pub struct StuckOutcome {
    pub killed: usize,
    pub completed_via_runtime: usize,
    pub retried: usize,
    pub reviews_bounced: usize,
}

pub async fn process_sessions(
    store: &Store,
    project_id: Uuid,
    snap: &BoardSnapshot,
    heartbeat_stale_s: i32,
    spawn_timeout_s: i32,
    kill_grace_s: i32,
    default_task_timeout_s: i32,
    cancel_by_run: &HashMap<Uuid, bool>,
    data_dir: &Path,
) -> Result<StuckOutcome, TickError> {
    let mut out = StuckOutcome {
        killed: 0,
        completed_via_runtime: 0,
        retried: 0,
        reviews_bounced: 0,
    };
    let now = Utc::now();
    let tasks: HashMap<Uuid, _> = snap.tasks.iter().map(|t| (t.id, t)).collect();

    for session in &snap.sessions {
        let Some(st) = WorkerSessionStatus::from_db(&session.status) else {
            continue;
        };
        if matches!(
            st,
            WorkerSessionStatus::Exited | WorkerSessionStatus::FailedSpawn
        ) {
            continue;
        }
        let task = tasks.get(&session.board_task_id);
        let task_terminal = task
            .map(|t| {
                matches!(
                    t.status.as_str(),
                    "done_success" | "done_failed" | "cancelled"
                )
            })
            .unwrap_or(false);

        let remote = session.runtime_backend == "remote";

        // Terminal task still holding session → free
        if task_terminal {
            store
                .free_session_claim(session.board_task_id, session.id, "clean")
                .await?;
            continue;
        }

        // Legacy in-process sessions are not completed on the plane.
        if !remote {
            continue;
        }

        // Spawn hang. Remote stays spawning until the host claims + job heartbeats.
        if st == WorkerSessionStatus::Spawning {
            let limit = if remote {
                spawn_timeout_s.max(300)
            } else {
                spawn_timeout_s
            };
            let age = now.signed_duration_since(session.created_at);
            if age > ChronoDuration::seconds(limit as i64) {
                store.complete_spawn_failure(session.id).await?;
                let max_retries = task.map(|t| t.max_retries).unwrap_or(2);
                let cancel = cancel_by_run.get(&session.run_id).copied().unwrap_or(false);
                apply_retry_after_failure(
                    store,
                    session.board_task_id,
                    session.attempt,
                    max_retries,
                    cancel,
                    ExitReason::SpawnFailed,
                )
                .await?;
                out.retried += 1;
                continue;
            }
        }

        // Killing grace expired
        if st == WorkerSessionStatus::Killing {
            let past = session
                .kill_grace_deadline
                .map(|d| now >= d)
                .unwrap_or(true);
            if past {
                let cancel = cancel_by_run.get(&session.run_id).copied().unwrap_or(false);
                let reason = session.exit_reason.as_deref().unwrap_or("stale");
                store
                    .free_session_claim(session.board_task_id, session.id, reason)
                    .await?;
                if cancel || reason == "cancel" {
                    store
                        .set_task_cancelled(session.board_task_id, "cancel")
                        .await?;
                } else {
                    let max_retries = task.map(|t| t.max_retries).unwrap_or(2);
                    let exit = match reason {
                        "timeout" => ExitReason::Timeout,
                        "spawn_hang" => ExitReason::SpawnHang,
                        _ => ExitReason::Stale,
                    };
                    apply_retry_after_failure(
                        store,
                        session.board_task_id,
                        session.attempt,
                        max_retries,
                        cancel,
                        exit,
                    )
                    .await?;
                    out.retried += 1;
                }
                out.killed += 1;
            }
            continue;
        }

        // Stuck predicates for healthy/stuck
        if matches!(
            st,
            WorkerSessionStatus::Healthy | WorkerSessionStatus::Stuck
        ) {
            let hb_base = session
                .last_heartbeat_at
                .or(session.started_at)
                .unwrap_or(session.created_at);
            let mut stale = now.signed_duration_since(hb_base)
                > ChronoDuration::seconds(heartbeat_stale_s as i64);
            if stale {
                let work = session_work_dir(data_dir, session, project_id);
                if heartbeat_file_fresh(&work, now, heartbeat_stale_s) {
                    stale = false;
                }
            }

            let timeout_s = task
                .and_then(|t| t.timeout_s)
                .unwrap_or(default_task_timeout_s);
            let wall = session
                .started_at
                .map(|s| now.signed_duration_since(s) > ChronoDuration::seconds(timeout_s as i64))
                .unwrap_or(false);

            let cancel = cancel_by_run.get(&session.run_id).copied().unwrap_or(false);
            if cancel || stale || wall {
                let exit_reason = if cancel {
                    "cancel"
                } else if wall {
                    "timeout"
                } else {
                    "stale"
                };
                if st == WorkerSessionStatus::Healthy {
                    let _ = store.mark_session_stuck(session.id).await;
                }
                let grace = now + ChronoDuration::seconds(kill_grace_s as i64);
                store
                    .mark_session_killing(session.id, exit_reason, grace)
                    .await?;
                if let Some(host_id) = crate::sandbox::remote::host_id_from_handle(
                    session.runtime_handle.as_deref().unwrap_or(""),
                ) {
                    let _ = crate::store::enqueue_signal_session(
                        store, project_id, session.id, host_id, "kill",
                    )
                    .await;
                }
                if kill_grace_s <= 0 {
                    store
                        .free_session_claim(session.board_task_id, session.id, exit_reason)
                        .await?;
                    if cancel {
                        store
                            .set_task_cancelled(session.board_task_id, "cancel")
                            .await?;
                    } else {
                        let max_retries = task.map(|t| t.max_retries).unwrap_or(2);
                        apply_retry_after_failure(
                            store,
                            session.board_task_id,
                            session.attempt,
                            max_retries,
                            cancel,
                            if wall {
                                ExitReason::Timeout
                            } else {
                                ExitReason::Stale
                            },
                        )
                        .await?;
                        out.retried += 1;
                    }
                    out.killed += 1;
                }
            }
        }
    }
    Ok(out)
}

fn heartbeat_file_fresh(work: &Path, now: chrono::DateTime<Utc>, stale_s: i32) -> bool {
    let p = work.join(".heartbeat");
    let Ok(meta) = std::fs::metadata(&p) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    let mtime = chrono::DateTime::<Utc>::from(mtime);
    now.signed_duration_since(mtime) <= ChronoDuration::seconds(stale_s as i64)
}

fn session_work_dir(
    data_dir: &Path,
    session: &crate::store::WorkerSessionRow,
    project_id: Uuid,
) -> PathBuf {
    if let Some(w) = session.work_dir.as_deref().filter(|s| !s.is_empty()) {
        return PathBuf::from(w);
    }
    data_dir
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(session.id.to_string())
}
