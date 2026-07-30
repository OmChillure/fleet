use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

/// Durable work unit for the agent claim queue (`work_claims`).
///
/// Claim with `FOR UPDATE SKIP LOCKED` so concurrent agents never deadlock
/// waiting on the same ready row.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct WorkClaim {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub payload: JsonValue,
    pub status: String,
    pub lease_owner: Option<Uuid>,
    pub lease_token: Option<Uuid>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub available_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct NewWorkClaim {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub payload: JsonValue,
    pub max_attempts: i32,
    pub available_at: DateTime<Utc>,
}
