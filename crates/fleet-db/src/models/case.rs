use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Durable general-purpose case (backend state machine; not coding-specific).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Case {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub title: String,
    pub policy_md: String,
    pub status: String,
    pub stage: String,
    pub context: Value,
    pub next_wake_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub run_id: Option<Uuid>,
    pub decision_count: i32,
    pub created_by: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct CaseEvent {
    pub id: Uuid,
    pub case_id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}
