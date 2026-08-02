use axum::extract::FromRef;
use fleet_auth::{AuthConfig, AuthState};
use fleet_db::PgPool;

use crate::config::IntegrationsConfig;

/// State for integration HTTP handlers.
#[derive(Clone)]
pub struct IntegrationsState {
    pub db: PgPool,
    pub auth: AuthConfig,
    pub config: IntegrationsConfig,
}

impl FromRef<IntegrationsState> for AuthState {
    fn from_ref(state: &IntegrationsState) -> AuthState {
        AuthState {
            db: state.db.clone(),
            config: state.auth.clone(),
        }
    }
}

impl IntegrationsState {
    pub fn auth_state(&self) -> AuthState {
        AuthState {
            db: self.db.clone(),
            config: self.auth.clone(),
        }
    }
}
