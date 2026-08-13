//! Unified Agent Fleet HTTP API.
//!
//! Auth + workspaces + integrations + orchestrator control plane on one process.
//! The binary (`src/main.rs`) binds a port; tests use [`build_http_app`] without serving.

pub mod cases;
pub mod chat;
pub mod chat_tools;
pub mod config;
pub mod fleet_routes;
pub mod grok;
pub mod retry;
pub mod state;
pub mod tickets;

pub use chat::OllamaConfig;
pub use config::ServerConfig;
pub use grok::GrokConfig;
pub use state::AppState;

use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use fleet_orchestrator::api::router as orch_router;
use serde_json::{json, Value};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, TraceLayer};
use tracing::{Level, Span};

/// Fleet-server routes only (auth, workspaces, integrations, chat, fleet BFF).
///
/// Does not attach orchestrator admin/worker routes, CORS, or tracing — see
/// [`build_http_app`] for the full HTTP stack used in production and tests.
pub fn build_router() -> Router<AppState> {
    Router::new()
        .route("/", get(root_index))
        .route("/health", get(health))
        .route("/api", get(api_index))
        .merge(fleet_auth::router::<AppState>())
        // Integrations catalog (GitHub, Jira, Slack, …)
        .merge(fleet_integrations::router::<AppState>())
        // Workspace chat → Grok (xAI) + Ollama
        .merge(chat::router())
        // Durable general-purpose cases (state machine + wake loop)
        .merge(cases::router())
        // Workspace-scoped fleet / orchestrator UI routes
        .merge(fleet_routes::router())
}

/// Full HTTP app: fleet-server routes + orchestrator fallback + TraceLayer + CORS.
///
/// Does **not** start the background ticker — the binary (`run`) does that;
/// tests tick explicitly.
pub fn build_http_app(
    state: AppState,
    orch: fleet_orchestrator::api::AppState,
    config: &ServerConfig,
) -> anyhow::Result<Router> {
    // Admin / worker bearer routes remain on orch_router (/v1, /healthz).
    let orch_service = orch_router(orch);

    Ok(build_router()
        .with_state(state)
        .fallback_service(orch_service)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    DefaultMakeSpan::new()
                        .level(Level::INFO)
                        .include_headers(false),
                )
                // Request start is debug-only; success shows at info (normal access log).
                .on_request(DefaultOnRequest::new().level(Level::DEBUG))
                .on_response(|response: &Response, latency: Duration, _span: &Span| {
                    let status = response.status();
                    let latency_ms = latency.as_millis();
                    if status.is_server_error() {
                        tracing::error!(%status, latency_ms, "http response");
                    } else if status.is_client_error() {
                        tracing::warn!(%status, latency_ms, "http response");
                    } else {
                        // Normal happy-path traffic — visible with RUST_LOG=info
                        tracing::info!(%status, latency_ms, "http response");
                    }
                })
                .on_failure(
                    |error: ServerErrorsFailureClass, latency: Duration, _span: &Span| {
                        tracing::error!(
                            error = %error,
                            latency_ms = latency.as_millis(),
                            "http request failed"
                        );
                    },
                ),
        )
        .layer(build_cors(config)?))
}

pub async fn root_index(headers: HeaderMap) -> Response {
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
            "personas": "/api/workspaces/{workspace_id}/personas/*",
            "integrations": "/api/workspaces/{workspace_id}/integrations/*",
            "chat": "/api/workspaces/{workspace_id}/chat/*",
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

pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = match state.db.acquire().await {
        Ok(_) => true,
        Err(e) => {
            tracing::error!(error = %e, "health check: database acquire failed");
            false
        }
    };
    if !db_ok {
        tracing::warn!("health check degraded: database down");
    }
    // Compile-time only (build.rs reruns on FLEET_GIT_SHA). Runtime ENV /
    // `kubectl set env` must not be able to make an old binary look like HEAD.
    let git_sha = option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "fleet-server",
        "db": if db_ok { "up" } else { "down" },
        "git_sha": git_sha,
        "apis": ["auth", "workspaces", "personas", "integrations", "chat", "cases", "fleet", "orchestrator"],
    }))
}

pub async fn api_index() -> Json<Value> {
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
            "personas": {
                "list": "GET /api/workspaces/{workspace_id}/personas",
                "create": "POST /api/workspaces/{workspace_id}/personas",
                "get": "GET /api/workspaces/{workspace_id}/personas/{persona_id}",
                "patch": "PATCH /api/workspaces/{workspace_id}/personas/{persona_id}",
                "delete": "DELETE /api/workspaces/{workspace_id}/personas/{persona_id}",
                "seed_starters": "POST /api/workspaces/{workspace_id}/personas/seed-starters",
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
                "sessions": "GET /api/workspaces/{workspace_id}/fleet/sessions",
                "workers": "GET /api/workspaces/{workspace_id}/fleet/workers",
            },
            "cases": {
                "list": "GET /api/workspaces/{workspace_id}/cases",
                "create": "POST /api/workspaces/{workspace_id}/cases",
                "get": "GET /api/workspaces/{workspace_id}/cases/{case_id}",
                "observe": "POST /api/workspaces/{workspace_id}/cases/{case_id}/observe",
                "cancel": "POST /api/workspaces/{workspace_id}/cases/{case_id}/cancel",
                "decide": "POST /api/workspaces/{workspace_id}/cases/{case_id}/decide",
            },
            "chat": {
                "status": "GET /api/workspaces/{workspace_id}/chat/status",
                "conversations": "GET|POST /api/workspaces/{workspace_id}/chat/conversations",
                "conversation": "GET|DELETE /api/workspaces/{workspace_id}/chat/conversations/{id}",
                "complete": "POST /api/workspaces/{workspace_id}/chat",
                "note": "Grok CLI + Ollama; persisted history; fleet + integration tools",
            },
            "orchestrator_admin": {
                "note": "Bearer ADMIN_TOKEN for automation / workers",
                "projects": "POST /v1/projects",
                "tick": "POST /v1/projects/{id}/admin/tick",
            }
        }
    }))
}

pub fn build_cors(config: &ServerConfig) -> anyhow::Result<CorsLayer> {
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
