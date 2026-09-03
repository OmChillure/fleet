//! Slack (and optional env channel) notifications via the control-plane proxy.

use async_trait::async_trait;
use serde_json::json;

use super::{HitlBlockedEvent, NotificationSink, NotifyError};
use crate::sandbox::io::IntegrationHandle;

pub struct IoNotify {
    pub io: IntegrationHandle,
}

impl IoNotify {
    pub fn new(io: IntegrationHandle) -> Self {
        Self { io }
    }
}

fn slack_channel() -> Option<String> {
    std::env::var("FLEET_NOTIFY_SLACK_CHANNEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[async_trait]
impl NotificationSink for IoNotify {
    async fn on_blocked(&self, event: HitlBlockedEvent) -> Result<(), NotifyError> {
        let text = format!(
            "Fleet task blocked — needs a human.\nProject `{}` · run `{}` · task `{}`\n{}",
            event.project_id, event.run_id, event.board_task_id, event.reason
        );
        post_slack(&self.io, event.project_id, &text).await
    }

    async fn on_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<(), NotifyError> {
        if !matches!(
            event_type,
            "run_completed" | "run_failed" | "run_cancelled" | "run_terminal"
        ) {
            return Ok(());
        }
        let text = format!("Fleet {event_type}: {payload}");
        let ws = payload
            .get("project_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        let Some(ws) = ws else {
            return Ok(());
        };
        post_slack(&self.io, ws, &text).await
    }
}

async fn post_slack(
    io: &IntegrationHandle,
    workspace_id: uuid::Uuid,
    text: &str,
) -> Result<(), NotifyError> {
    let Some(inv) = io.get() else {
        return Ok(());
    };
    let Some(channel) = slack_channel() else {
        tracing::debug!("FLEET_NOTIFY_SLACK_CHANNEL unset; skip Slack notify");
        return Ok(());
    };
    let res = inv
        .invoke(
            workspace_id,
            "slack".into(),
            "post_message".into(),
            json!({ "channel": channel, "text": text }),
        )
        .await;
    if !res.ok {
        return Err(NotifyError::Message(res.summary));
    }
    Ok(())
}
