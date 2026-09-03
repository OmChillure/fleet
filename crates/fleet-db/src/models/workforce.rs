use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Durable outcome. Agents come and go; this row is the workflow.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct WorkforceGoal {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub goal_md: String,
    pub status: String,
    pub run_id: Option<Uuid>,
    pub case_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Human gate for a write the agent is not allowed to fire alone.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PolicyApproval {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub kind: String,
    pub provider: String,
    pub operation: String,
    pub args: Value,
    pub summary: String,
    pub status: String,
    pub result: Option<Value>,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
