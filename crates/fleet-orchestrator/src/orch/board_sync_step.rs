//! PR10 — push board task status to ExternalBoardSync when enabled.
//!
//! Gated by `project_configs.auto_sync_external`. No Jira/Linear client —
//! adapters are Noop or Log (and tests may inject a recording sink).

use crate::store::BoardSnapshot;
use crate::sync::{ExternalBoardSync, TaskStatusSync};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ExternalRefJson {
    provider: String,
    key: String,
}

/// Sync every task that has an `external_ref`. Returns number of successful sync calls.
pub async fn sync_external_board(
    sync: &Arc<dyn ExternalBoardSync>,
    snap: &BoardSnapshot,
    enabled: bool,
) -> usize {
    if !enabled {
        return 0;
    }
    let mut n = 0usize;
    for task in &snap.tasks {
        let Some(ref_val) = task.external_ref.as_ref() else {
            continue;
        };
        let ext: ExternalRefJson = match serde_json::from_value(ref_val.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "external_ref parse failed; skip board sync"
                );
                continue;
            }
        };
        let item = TaskStatusSync {
            board_task_id: task.id,
            external_provider: Some(ext.provider),
            external_key: Some(ext.key),
            status: task.status.clone(),
        };
        match sync.sync_task_status(item).await {
            Ok(()) => n += 1,
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "external board sync failed"
                );
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{BoardTaskRow, WorkerSessionRow};
    use crate::sync::SyncError;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::Mutex;
    use uuid::Uuid;

    struct RecordingSync {
        items: Mutex<Vec<TaskStatusSync>>,
    }

    #[async_trait]
    impl ExternalBoardSync for RecordingSync {
        async fn sync_task_status(&self, item: TaskStatusSync) -> Result<(), SyncError> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    fn task_with_ext(status: &str, provider: &str, key: &str) -> BoardTaskRow {
        let now = Utc::now();
        BoardTaskRow {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            plan_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            title: "T".into(),
            summary: String::new(),
            spec_md: String::new(),
            status: status.into(),
            depends_on: vec![],
            priority: 0,
            worker_profile: "coding".into(),
            acceptance: serde_json::json!([]),
            external_ref: Some(serde_json::json!({ "provider": provider, "key": key })),
            timeout_s: None,
            max_retries: 2,
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

    #[tokio::test]
    async fn skips_when_disabled() {
        let sync: Arc<dyn ExternalBoardSync> = Arc::new(RecordingSync {
            items: Mutex::new(vec![]),
        });
        let snap = BoardSnapshot {
            tasks: vec![task_with_ext("ready", "jira", "AF-1")],
            sessions: Vec::<WorkerSessionRow>::new(),
        };
        assert_eq!(sync_external_board(&sync, &snap, false).await, 0);
    }

    #[tokio::test]
    async fn syncs_tasks_with_external_ref() {
        let rec = Arc::new(RecordingSync {
            items: Mutex::new(vec![]),
        });
        let sync: Arc<dyn ExternalBoardSync> = rec.clone();
        let t1 = task_with_ext("in_progress", "jira", "AF-1");
        let mut t2 = task_with_ext("queued", "linear", "LIN-9");
        t2.external_ref = None;
        let snap = BoardSnapshot {
            tasks: vec![t1.clone(), t2],
            sessions: vec![],
        };
        assert_eq!(sync_external_board(&sync, &snap, true).await, 1);
        let items = rec.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].external_provider.as_deref(), Some("jira"));
        assert_eq!(items[0].external_key.as_deref(), Some("AF-1"));
        assert_eq!(items[0].status, "in_progress");
        assert_eq!(items[0].board_task_id, t1.id);
    }
}
