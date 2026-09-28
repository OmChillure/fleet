//! Integrations feature (`/integrations` + `/oauth/done` popup).

mod api;
mod logos;
mod oauth_done;
mod page;

pub use api::{
    find_integration, granted_repos, integration_is_connected, list_integrations, oauth_start_url,
};
pub use logos::IntegrationLogo;
pub use oauth_done::OAuthDone;
pub use page::Integrations;
