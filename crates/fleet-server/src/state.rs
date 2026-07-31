use axum::extract::FromRef;
use fleet_auth::{AuthConfig, AuthState};
use fleet_db::PgPool;
use fleet_integrations::{IntegrationsConfig, IntegrationsState};

/// Unified app state for every API module mounted on fleet-server.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub auth: AuthConfig,
    pub integrations: IntegrationsConfig,
}

impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> AuthState {
        AuthState {
            db: state.db.clone(),
            config: state.auth.clone(),
        }
    }
}

impl FromRef<AppState> for IntegrationsState {
    fn from_ref(state: &AppState) -> IntegrationsState {
        IntegrationsState {
            db: state.db.clone(),
            auth: state.auth.clone(),
            config: state.integrations.clone(),
        }
    }
}

impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
