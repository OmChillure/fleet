//! Unified Agent Fleet HTTP API.
//!
//! Auth + workspaces + integrations + orchestrator control plane on one process.

mod config;
mod fleet_routes;
mod state;

use std::net::SocketAddr;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
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
                "info,sqlx=warn,fleet_db=info,fleet_auth=info,fleet_integrations=info,fleet_server=info,fleet_orchestrator=info",
            )
        }))
        .init();

    let config = ServerConfig::from_env()?;
    validate_cookie_policy(&config.auth);

    let db = connect_and_migrate(&config.database_url).await?;

    let orch_cfg = OrchConfig::from_env_for_server(&config.database_url, &config.bind.to_string())?;
    let orch = build_app_state(&orch_cfg).await?;

    let state = AppState {
        db,
        auth: config.auth.clone(),
        integrations: config.integrations.clone(),
        orch: orch.clone(),
    };

    // Admin / worker bearer routes remain on orch_router (/v1, /healthz).
    let orch_service = orch_router(orch);

    let app = build_router()
        .with_state(state)
        .fallback_service(orch_service)
        .layer(TraceLayer::new_for_http())
        .layer(build_cors(&config)?);

    let addr: SocketAddr = config.bind;
    tracing::info!(
        %addr,
        "fleet-server listening (auth + workspaces + integrations + fleet + orchestrator)"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_router() -> Router<AppState> {
    Router::new()
        .route("/", get(root_index))
        .route("/health", get(health))
        .route("/api", get(api_index))
        .merge(fleet_auth::router::<AppState>())
        // Integrations catalog (GitHub, Jira, Slack, …)
        .merge(fleet_integrations::router::<AppState>())
        // Workspace-scoped fleet / orchestrator UI routes
        .merge(fleet_routes::router())
}

async fn root_index(headers: HeaderMap) -> Response {
    let body = json!({
        "service": "fleet-server",
        "status": "ok",
        "message": "Agent Fleet unified API",
        "links": {
            "health": "/health",
            "healthz": "/healthz",
            "api_index": "/api",
            "ui": "http://localhost:3030",
            "fleet": "/api/workspaces/{workspace_id}/fleet",
        },
        "apis": {
            "auth": "/api/auth/*",
            "workspaces": "/api/workspaces",
            "integrations": "/api/workspaces/{workspace_id}/integrations/*",
            "fleet": "/api/workspaces/{workspace_id}/fleet/*",
            "orchestrator_admin": "/v1/*",
        }
    });

    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept.contains("text/html") {
        let pretty = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
        Html(format!(
            r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"/><title>Agent Fleet API</title>
<style>
body{{font-family:system-ui,sans-serif;max-width:42rem;margin:2rem auto;padding:0 1rem;background:#f4f6fa;color:#1a1d26}}
a{{color:#2563eb}} pre{{background:#0f172a;color:#e2e8f0;padding:1rem;border-radius:10px;overflow:auto;font-size:.82rem}}
.ok{{color:#0d7a4f;font-weight:600}}
</style></head><body>
<h1>Agent Fleet API</h1>
<p class="ok">status: ok</p>
<p>Use the UI at <a href="http://localhost:3030">localhost:3030</a>. Fleet routes are cookie-authenticated under each workspace.</p>
<ul>
<li><a href="/health">/health</a></li>
<li><a href="/healthz">/healthz</a></li>
<li><a href="/api">/api</a></li>
</ul>
<pre>{pretty}</pre>
</body></html>"#
        ))
        .into_response()
    } else {
        (StatusCode::OK, Json(body)).into_response()
    }
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = state.db.acquire().await.is_ok();
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "fleet-server",
        "db": if db_ok { "up" } else { "down" },
        "apis": ["auth", "workspaces", "integrations", "fleet", "orchestrator"],
    }))
}

async fn api_index() -> Json<Value> {
    Json(json!({
        "service": "fleet-server",
        "apis": {
            "auth": {
                "signup": "POST /api/auth/signup",
                "login": "POST /api/auth/login",
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
            },
            "fleet": {
                "project": "GET /api/workspaces/{workspace_id}/fleet",
                "fixture": "POST /api/workspaces/{workspace_id}/fleet/runs/fixture",
                "tick": "POST /api/workspaces/{workspace_id}/fleet/tick",
                "tasks": "GET /api/workspaces/{workspace_id}/fleet/tasks",
                "runs": "GET /api/workspaces/{workspace_id}/fleet/runs",
            },
            "orchestrator_admin": {
                "note": "Bearer ADMIN_TOKEN for automation / workers",
                "projects": "POST /v1/projects",
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
