use axum::extract::FromRef;
use fleet_db::PgPool;

use crate::config::AuthConfig;

/// State required by auth handlers. Extracted from the server `AppState` via [`FromRef`].
#[derive(Clone)]
pub struct AuthState {
    pub db: PgPool,
    pub config: AuthConfig,
}

impl FromRef<AuthState> for AuthConfig {
    fn from_ref(state: &AuthState) -> Self {
        state.config.clone()
    }
}

impl FromRef<AuthState> for PgPool {
    fn from_ref(state: &AuthState) -> Self {
        state.db.clone()
    }
}
