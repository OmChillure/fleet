//! Unified Agent Fleet HTTP API.
//!
//! ```bash
//! # from workspace root (with `.env`)
//! cargo run
//! # → http://localhost:8080
//! ```
//!
//! Mounts auth/workspaces and the orchestrator control plane (`/v1`, `/healthz`).

mod config;
mod state;

use std::net::SocketAddr;

use axum::extract::State;
use axum::http::{header, HeaderValue, Method};
use axum::routing::get;
use axum::{Json, Router};
use fleet_auth::validate_cookie_policy;
use fleet_db::connect_and_migrate;
use fleet_orchestrator::api::{build_app_state, router as orch_router};
use fleet_orchestrator::config::Config as OrchConfig;
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::ServerConfig;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "info,sqlx=warn,fleet_db=info,fleet_auth=info,fleet_server=info,fleet_orchestrator=info",
            )
        }))
        .init();

    let config = ServerConfig::from_env()?;
    validate_cookie_policy(&config.auth);

    let db = connect_and_migrate(&config.database_url).await?;

    let state = AppState {
        db,
        auth: config.auth.clone(),
    };

    // Orchestrator control plane (same process). Prefer ORCHESTRATOR_DATABASE_URL
    // only if you must isolate DBs; default shares DATABASE_URL (migrations co-exist).
    let orch_cfg = OrchConfig::from_env_for_server(&config.database_url, &config.bind.to_string())?;
    let orch_state = build_app_state(&orch_cfg).await?;
    let orch = orch_router(orch_state);

    let app = build_router()
        .with_state(state)
        // Unmatched routes fall through to orchestrator (/v1/*, /healthz, /readyz).
        .fallback_service(orch)
        .layer(TraceLayer::new_for_http())
        .layer(build_cors(&config)?);

    let addr: SocketAddr = config.bind;
    tracing::info!(
        %addr,
        "fleet-server listening (auth + workspaces + orchestrator)"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Compose domain APIs that share AppState.
fn build_router() -> Router<AppState> {
    Router::new()
        .route("/", get(root_index))
        .route("/health", get(health))
        .route("/api", get(api_index))
        .merge(fleet_auth::router::<AppState>())
}

/// Browser-friendly landing for `GET /` (was empty 404).
async fn root_index() -> Json<Value> {
    Json(json!({
        "service": "fleet-server",
        "status": "ok",
        "message": "Agent Fleet unified API",
        "links": {
            "health": "/health",
            "healthz": "/healthz",
            "readyz": "/readyz",
            "api_index": "/api",
            "ui": "http://localhost:3030",
        },
        "apis": {
            "auth": "/api/auth/*",
            "workspaces": "/api/workspaces",
            "orchestrator": "/v1/*",
        }
    }))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = state.db.acquire().await.is_ok();
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "fleet-server",
        "db": if db_ok { "up" } else { "down" },
        "apis": ["auth", "workspaces", "orchestrator"],
    }))
}

async fn api_index() -> Json<Value> {
    Json(json!({
        "service": "fleet-server",
        "apis": {
            "auth": {
                "signup": "POST /api/auth/signup",
                "login": "POST /api/auth/login",
                "logout": "POST /api/auth/logout",
                "refresh": "POST /api/auth/refresh",
                "me": "GET /api/auth/me",
            },
            "workspaces": {
                "list": "GET /api/workspaces",
                "create": "POST /api/workspaces",
            },
            "orchestrator": {
                "healthz": "GET /healthz",
                "projects": "POST /v1/projects",
                "fixture": "POST /v1/projects/{id}/runs/fixture",
                "tick": "POST /v1/projects/{id}/admin/tick",
            }
        }
    }))
}

fn build_cors(config: &ServerConfig) -> anyhow::Result<CorsLayer> {
    let origins: Vec<HeaderValue> = config
        .cors_origins
        .iter()
        .map(|o| o.parse::<HeaderValue>())
        .collect::<Result<_, _>>()?;

    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::AUTHORIZATION,
            header::COOKIE,
        ])
        .allow_credentials(true)
        .expose_headers([header::SET_COOKIE]))
}
