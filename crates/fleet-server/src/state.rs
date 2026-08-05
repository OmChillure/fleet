use axum::extract::FromRef;
use fleet_auth::{AuthConfig, AuthState};
use fleet_db::PgPool;
use fleet_integrations::{IntegrationsConfig, IntegrationsState};
use fleet_orchestrator::api::AppState as OrchState;

use crate::chat::OllamaConfig;

/// Unified app state for every API module mounted on fleet-server.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub auth: AuthConfig,
    pub integrations: IntegrationsConfig,
    /// Orchestrator control plane (project store + tick runner).
    pub orch: OrchState,
    /// Local Ollama for workspace chat.
    pub ollama: OllamaConfig,
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

impl FromRef<AppState> for OrchState {
    fn from_ref(state: &AppState) -> Self {
        state.orch.clone()
    }
}
