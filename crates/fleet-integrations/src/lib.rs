//! Integrations — users **OAuth-connect** GitHub, Slack, Jira, Linear, Figma,
//! Notion, Google Drive, Teams, Discord, … to a workspace.
//! Credentials stay in the control plane for AI use; never enter sandboxes.
//! OAuth tokens are encrypted at rest (AES-256-GCM) before Postgres storage.
//!
//! | Method | Path | Role |
//! | --- | --- | --- |
//! | `GET` | `/api/workspaces/:id/integrations` | Catalog + connection state |
//! | `GET` | `/api/workspaces/:id/integrations/:provider/oauth/start` | 302 → provider |
//! | `GET` | `/api/integrations/oauth/callback/:provider` | Code exchange → store → UI |
//! | `DELETE` | `/api/workspaces/:id/integrations/:provider` | Disconnect |

#![forbid(unsafe_code)]

mod catalog;
mod config;
mod crypto;
mod models;
mod oauth;
mod routes;
mod state;

pub use catalog::{catalog, find_provider, is_known_provider};
pub use config::{IntegrationsConfig, OAuthClientConfig};
pub use crypto::CredentialsKey;
pub use models::{
    AuthMethod, DisconnectResponse, IntegrationAvailability, IntegrationCategory,
    IntegrationListResponse, IntegrationView,
};
pub use routes::router;
pub use state::IntegrationsState;

use fleet_auth::{AppError, AppResult};
use fleet_db::PgPool;
use serde_json::Value;
use uuid::Uuid;

/// Load **decrypted** credentials for the orchestrator / AI proxy (internal).
///
/// Tokens never leave the control plane; this is the only supported way to
/// read them for outbound API calls.
pub async fn credentials_for(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    key: &CredentialsKey,
) -> AppResult<Option<Value>> {
    let raw = fleet_db::repos::integrations::credentials_for(pool, workspace_id, provider)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    match raw {
        None => Ok(None),
        Some(stored) => {
            let plain = key
                .decrypt(&stored)
                .map_err(|e| AppError::internal(format!("decrypt credentials: {e}")))?;
            Ok(Some(plain))
        }
    }
}
