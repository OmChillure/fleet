use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Full row including credentials — control-plane / repo only.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IntegrationConnection {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub status: String,
    pub account_label: Option<String>,
    pub config: Value,
    /// Secret tokens — never put this on a public API type.
    pub credentials: Value,
    pub connected_by: Option<Uuid>,
    pub last_error: Option<String>,
    pub connected_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// Safe subset for HTTP responses (no credentials).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationConnectionPublic {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub status: String,
    pub account_label: Option<String>,
    pub config: Value,
    pub connected: bool,
    pub last_error: Option<String>,
    pub connected_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&IntegrationConnection> for IntegrationConnectionPublic {
    fn from(c: &IntegrationConnection) -> Self {
        Self {
            id: c.id,
            workspace_id: c.workspace_id,
            provider: c.provider.clone(),
            status: c.status.clone(),
            account_label: c.account_label.clone(),
            config: c.config.clone(),
            connected: c.status == "connected",
            last_error: c.last_error.clone(),
            connected_at: c.connected_at,
            updated_at: c.updated_at,
        }
    }
}

impl From<IntegrationConnection> for IntegrationConnectionPublic {
    fn from(c: IntegrationConnection) -> Self {
        IntegrationConnectionPublic::from(&c)
    }
}
