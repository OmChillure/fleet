//! HITL / operator notification sink (PR4).

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HitlBlockedEvent {
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub board_task_id: Uuid,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("{0}")]
    Message(String),
}

#[async_trait]
pub trait NotificationSink: Send + Sync {
    async fn on_blocked(&self, event: HitlBlockedEvent) -> Result<(), NotifyError>;
    async fn on_event(&self, event_type: &str, payload: Value) -> Result<(), NotifyError>;
}

/// No-op sink for tests and early ticks.
pub struct NoopNotify;

#[async_trait]
impl NotificationSink for NoopNotify {
    async fn on_blocked(&self, _event: HitlBlockedEvent) -> Result<(), NotifyError> {
        Ok(())
    }

    async fn on_event(&self, _event_type: &str, _payload: Value) -> Result<(), NotifyError> {
        Ok(())
    }
}

/// Logs via tracing (usable in demos before webhook PR8).
pub struct LogNotify;

#[async_trait]
impl NotificationSink for LogNotify {
    async fn on_blocked(&self, event: HitlBlockedEvent) -> Result<(), NotifyError> {
        tracing::warn!(
            project_id = %event.project_id,
            run_id = %event.run_id,
            board_task_id = %event.board_task_id,
            reason = %event.reason,
            "HITL blocked"
        );
        Ok(())
    }

    async fn on_event(&self, event_type: &str, payload: Value) -> Result<(), NotifyError> {
        tracing::info!(event_type, %payload, "orchestration event");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_ok() {
        let n = NoopNotify;
        n.on_blocked(HitlBlockedEvent {
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            reason: "need secret".into(),
        })
        .await
        .unwrap();
        n.on_event("run_completed", serde_json::json!({"ok": true}))
            .await
            .unwrap();
    }
}
