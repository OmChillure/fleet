//! Unified Agent Fleet HTTP API.
//!
//! Auth + workspaces + integrations + orchestrator control plane on one process.
//! The binary (`src/main.rs`) binds a port; tests use [`build_http_app`] without serving.

pub mod admin;
pub mod bus;
pub mod cases;
pub mod chat;
pub mod chat_tools;
pub mod config;
pub mod fleet_routes;
pub mod grok;
pub mod inbox_sources;
pub mod inbox_webhooks;
pub mod install_routes;
pub mod llm_keys;
pub mod local_models;
pub mod package_versions;
pub mod partners;
pub mod planner;
pub mod providers;
pub mod runners;
pub mod retry;
pub mod runner_chat;
pub mod state;
pub mod tickets;
pub mod work_io;
pub mod worker_hosts;
pub mod workforce;

#[cfg(test)]
mod merge_gate;

pub use chat::OllamaConfig;
pub use config::ServerConfig;
pub use grok::GrokConfig;
pub use providers::{ClaudeConfig, GeminiConfig, OpenaiConfig};
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
        .merge(fleet_integrations::router::<AppState>())
        .merge(chat::router())
        .merge(cases::router())
        .merge(tickets::router())
        .merge(inbox_webhooks::router())
        .merge(workforce::router())
        .merge(fleet_routes::router())
        .merge(llm_keys::router())
        .merge(worker_hosts::router())
        .merge(partners::router())
        .merge(planner::router())
        .merge(runners::router())
        .merge(install_routes::router())
        .merge(admin::router())
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
        "apis": ["auth", "workspaces", "personas", "integrations", "chat", "cases", "fleet", "orchestrator", "worker_hosts", "admin"],
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
                "members": "GET /api/workspaces/{workspace_id}/members",
                "invite": "POST /api/workspaces/{workspace_id}/invites",
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
                "resume": "POST /api/workspaces/{workspace_id}/fleet/tasks/{task_id}/resume",
                "kill": "POST /api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill",
            },
            "admin": {
                "overview": "GET /api/admin/overview",
                "users": "GET /api/admin/users",
                "patch_user": "PATCH /api/admin/users/{user_id}",
                "workspaces": "GET /api/admin/workspaces",
                "runners": "GET /api/admin/runners",
                "instance": "GET /api/admin/instance",
                "work": "GET /api/admin/work",
                "sessions": "GET /api/admin/sessions",
            },
            "inbox": {
                "list": "GET /api/workspaces/{workspace_id}/inbox",
                "monitor": "POST /api/workspaces/{workspace_id}/inbox",
                "poll": "POST /api/workspaces/{workspace_id}/inbox/poll",
                "finish": "POST /api/workspaces/{workspace_id}/inbox/finish",
                "webhook": "POST /api/integrations/webhooks/{provider}/{hook_token}",
            },
            "cases": {
                "list": "GET /api/workspaces/{workspace_id}/cases",
                "create": "POST /api/workspaces/{workspace_id}/cases",
                "get": "GET /api/workspaces/{workspace_id}/cases/{case_id}",
                "observe": "POST /api/workspaces/{workspace_id}/cases/{case_id}/observe",
                "cancel": "POST /api/workspaces/{workspace_id}/cases/{case_id}/cancel",
                "decide": "POST /api/workspaces/{workspace_id}/cases/{case_id}/decide",
                "wake": "POST /api/workspaces/{workspace_id}/cases/{case_id}/wake",
            },
            "chat": {
                "status": "GET /api/workspaces/{workspace_id}/chat/status",
                "conversations": "GET|POST /api/workspaces/{workspace_id}/chat/conversations",
                "conversation": "GET|DELETE /api/workspaces/{workspace_id}/chat/conversations/{id}",
                "complete": "POST /api/workspaces/{workspace_id}/chat",
                "stream": "POST /api/workspaces/{workspace_id}/chat/stream",
                "note": "Grok / Ollama / OpenAI / Claude; NDJSON stream; fleet + integration tools",
            },
            "worker_hosts": {
                "local_models": "GET /api/workspaces/{workspace_id}/local-models",
                "local_model_tags": "GET /api/workspaces/{workspace_id}/local-models/{name}/tags",
                "list": "GET /api/workspaces/{workspace_id}/worker-hosts",
                "create": "POST /api/workspaces/{workspace_id}/worker-hosts",
                "get": "GET /api/workspaces/{workspace_id}/worker-hosts/{host_id}",
                "patch": "PATCH /api/workspaces/{workspace_id}/worker-hosts/{host_id}",
                "delete": "DELETE /api/workspaces/{workspace_id}/worker-hosts/{host_id}",
                "pair_tokens": "POST /api/workspaces/{workspace_id}/worker-hosts/{host_id}/pair-tokens",
                "pair": "POST /api/worker-hosts/pair",
                "heartbeat": "POST /api/worker-hosts/me/heartbeat",
                "lease": "POST /api/worker-hosts/me/lease",
                "complete": "POST /api/worker-hosts/me/lease/{claim_id}/complete",
                "install": "GET /install-worker.sh",
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
    // `*` is a literal no browser sends as Origin. tower-http's
    // `AllowOrigin::list` panics on a wildcard, and `AllowOrigin::any()`
    // cannot be combined with `allow_credentials(true)`. Drop `*` so a
    // `CORS_ORIGINS=*` env stays fail-closed instead of reflecting every
    // Origin or sending `Access-Control-Allow-Origin: *` with credentials.
    let origins: Vec<HeaderValue> = config
        .cors_origins
        .iter()
        .filter(|o| o.as_str() != "*")
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
