use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

/// Outbox row for a control-plane `BusMessage` (`control_plane_events`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct ControlPlaneEvent {
    pub id: Uuid,
    pub message_id: Uuid,
    pub event_type: String,
    pub body: JsonValue,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}
