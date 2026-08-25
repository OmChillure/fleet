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
    #[serde(default)]
    pub defaults: IntegrationAgentDefaults,
}

/// Runner / provider / model used when this integration launches work.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationAgentDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

impl IntegrationAgentDefaults {
    pub fn from_config(config: &Value) -> Self {
        Self {
            default_runner_id: config
                .get("default_runner_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            default_provider: config
                .get("default_provider")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            default_model: config
                .get("default_model")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        }
    }
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
            defaults: IntegrationAgentDefaults::from_config(&c.config),
        }
    }
}

/// Workspace-owned OAuth application (client credentials for starting a flow).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IntegrationOAuthApp {
    pub workspace_id: Uuid,
    pub provider: String,
    pub client_id: String,
    /// Encrypted envelope — never return on public APIs.
    pub client_secret: Value,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<IntegrationConnection> for IntegrationConnectionPublic {
    fn from(c: IntegrationConnection) -> Self {
        IntegrationConnectionPublic::from(&c)
    }
}
