//! Integrations feature (`/integrations` + `/oauth/done` popup).

mod api;
mod logos;
mod oauth_done;
mod connection;
mod page;

pub use connection::ConnectionEdit;
pub use api::{
    find_integration, granted_repos, integration_is_connected, integration_needs_reconnect,
    list_integrations, live_connections, load_integrations, message_is_auth_failure,
    oauth_start_url, IntegrationCategory,
};
pub use logos::IntegrationLogo;
pub use oauth_done::OAuthDone;
pub use page::Integrations;
