//! External board sync (PR4 / PR10). Log/Noop stubs + control-plane I/O adapter.

pub mod io;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TaskStatusSync {
    pub board_task_id: Uuid,
    pub project_id: Uuid,
    pub external_provider: Option<String>,
    pub external_key: Option<String>,
    pub status: String,
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("{0}")]
    Message(String),
}

#[async_trait]
pub trait ExternalBoardSync: Send + Sync {
    async fn sync_task_status(&self, item: TaskStatusSync) -> Result<(), SyncError>;
}

/// No-op external sync.
pub struct NoopBoardSync;

#[async_trait]
impl ExternalBoardSync for NoopBoardSync {
    async fn sync_task_status(&self, _item: TaskStatusSync) -> Result<(), SyncError> {
        Ok(())
    }
}

/// Logs sync intent without calling providers.
pub struct LogBoardSync;

#[async_trait]
impl ExternalBoardSync for LogBoardSync {
    async fn sync_task_status(&self, item: TaskStatusSync) -> Result<(), SyncError> {
        tracing::debug!(
            board_task_id = %item.board_task_id,
            status = %item.status,
            provider = ?item.external_provider,
            key = ?item.external_key,
            "external board sync stub"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_sync() {
        NoopBoardSync
            .sync_task_status(TaskStatusSync {
                board_task_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                external_provider: Some("jira".into()),
                external_key: Some("WEB-1".into()),
                status: "in_progress".into(),
            })
            .await
            .unwrap();
    }
}
