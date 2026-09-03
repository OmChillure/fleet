use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct TicketMonitor {
    pub workspace_id: Uuid,
    pub enabled: bool,
    pub providers: Vec<String>,
    pub github_repos: Vec<String>,
    pub jira_jql: Option<String>,
    pub slack_channels: Vec<String>,
    pub notion_databases: Vec<String>,
    pub auto_launch: bool,
    pub follow_connected: bool,
    pub filters: Value,
    pub launch_limit: i32,
    pub poll_limit: i32,
    pub hook_token: String,
    pub started_by: Option<Uuid>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct TicketInboxItem {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub external_key: String,
    pub title: String,
    pub url: Option<String>,
    pub state: String,
    pub summary: String,
    pub run_id: Option<Uuid>,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
