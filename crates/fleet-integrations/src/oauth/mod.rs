//! OAuth 2.0 authorization-code flows for connectable platforms.

mod github_app;
mod providers;
mod state_token;

pub use github_app::{convert_manifest, github_install_url, manifest_form_html};
pub use providers::{authorize_url, exchange_code};
pub use state_token::{issue_oauth_state, verify_oauth_state};
