use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

/// Append-only audit / domain event (`audit_events`). Never update rows.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub actor_user_id: Option<Uuid>,
    pub actor_kind: String,
    pub event_type: String,
    pub payload: JsonValue,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewAuditEvent {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub actor_user_id: Option<Uuid>,
    pub actor_kind: String,
    pub event_type: String,
    pub payload: JsonValue,
}
