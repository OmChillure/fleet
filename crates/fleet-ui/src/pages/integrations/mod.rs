//! Integrations feature (`/integrations` + `/oauth/done` popup).

mod api;
mod connection;
mod logos;
mod oauth_done;
mod page;

pub use api::{
    IntegrationCategory, find_integration, granted_repos, integration_is_connected,
    integration_needs_reconnect, list_integrations, live_connections, load_integrations,
    message_is_auth_failure, oauth_start_url,
};
pub use connection::ConnectionEdit;
pub use logos::IntegrationLogo;
pub use oauth_done::OAuthDone;
pub use page::Integrations;
