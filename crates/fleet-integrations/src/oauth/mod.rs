//! OAuth 2.0 authorization-code flows for connectable platforms.

mod callback_error;
mod github_app;
mod providers;
mod state_token;

pub use callback_error::OAuthCallbackError;
pub use github_app::{convert_manifest, github_install_url, manifest_form_html};
pub use providers::{authorize_url, authorize_url_at, exchange_code, exchange_code_at};
pub(crate) use providers::refresh_github_user_token;
pub use state_token::{
    issue_oauth_state, issue_oauth_state_named, verify_oauth_state, OAuthStateClaims,
};
