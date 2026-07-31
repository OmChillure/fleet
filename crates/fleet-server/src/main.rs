//! Unified Agent Fleet HTTP API.
//!
//! ```bash
//! # from workspace root (with `.env`)
//! cargo run
//! # → http://localhost:8080
//! ```
//!
//! Mounts all domain routers (auth today; more APIs later on the same process).

mod config;
mod state;

use std::net::SocketAddr;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, Method, header};
use axum::routing::get;
use axum::Router;
use fleet_auth::validate_cookie_policy;
use fleet_db::connect_and_migrate;
use serde_json::{Value, json};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::ServerConfig;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                EnvFilter::new(
                    "info,sqlx=warn,fleet_db=info,fleet_auth=info,fleet_integrations=info,fleet_server=info",
                )
            }),
        )
        .init();

    let config = ServerConfig::from_env()?;
    validate_cookie_policy(&config.auth);

    let db = connect_and_migrate(&config.database_url).await?;

    let state = AppState {
        db,
        auth: config.auth.clone(),
        integrations: config.integrations.clone(),
    };

    let app = build_router()
        .layer(TraceLayer::new_for_http())
        .layer(build_cors(&config)?)
        .with_state(state);

    let addr: SocketAddr = config.bind;
    tracing::info!(%addr, "fleet-server listening (all APIs)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Compose every domain API into one router.
fn build_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/api", get(api_index))
        // Auth (signup / login / logout / refresh / me)
        .merge(fleet_auth::router::<AppState>())
        // Integrations catalog (GitHub, Jira, Slack, …)
        .merge(fleet_integrations::router::<AppState>())
    // Future:
    // .merge(fleet_orchestrator::router::<AppState>())
    // .merge(fleet_sandboxes::router::<AppState>())
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = state.db.acquire().await.is_ok();
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "fleet-server",
        "db": if db_ok { "up" } else { "down" },
        "apis": ["auth", "workspaces", "integrations"],
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
            "integrations": {
                "list": "GET /api/workspaces/{workspace_id}/integrations",
                "oauth_start": "GET /api/workspaces/{workspace_id}/integrations/{provider}/oauth/start",
                "oauth_callback": "GET /api/integrations/oauth/callback/{provider}",
                "disconnect": "DELETE /api/workspaces/{workspace_id}/integrations/{provider}",
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
