//! Integrations — users **OAuth-connect** GitHub, Slack, Jira, Linear to a workspace.
//! Credentials stay in the control plane for AI use; never enter sandboxes.
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
mod models;
mod oauth;
mod routes;
mod state;

pub use catalog::{catalog, find_provider, is_known_provider};
pub use config::{IntegrationsConfig, OAuthClientConfig};
pub use models::{
    AuthMethod, DisconnectResponse, IntegrationAvailability, IntegrationCategory,
    IntegrationListResponse, IntegrationView,
};
pub use routes::router;
pub use state::IntegrationsState;

/// Control-plane helper for AI / orchestrator proxy.
pub use fleet_db::repos::integrations::credentials_for;
