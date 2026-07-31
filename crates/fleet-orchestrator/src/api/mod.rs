//! HTTP control plane (PR6). Admin bearer + worker session token auth.

mod auth;
mod handlers;
mod state;

pub use state::{build_app_state, AppState};

use axum::http::{header, HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use handlers::{
    admin_tick, cancel_run, create_project, fixture_run, get_config, get_project, get_task, healthz,
    list_runs, list_tasks, list_workers, post_heartbeat, post_status, readyz, resume_task,
};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/projects", post(create_project))
        .route("/v1/projects/{project_id}", get(get_project))
        .route("/v1/projects/{project_id}/config", get(get_config))
        .route("/v1/projects/{project_id}/runs/fixture", post(fixture_run))
        .route("/v1/projects/{project_id}/runs", get(list_runs))
        .route(
            "/v1/projects/{project_id}/runs/{run_id}/cancel",
            post(cancel_run),
        )
        .route("/v1/projects/{project_id}/tasks", get(list_tasks))
        .route("/v1/projects/{project_id}/tasks/{task_id}", get(get_task))
        .route(
            "/v1/projects/{project_id}/tasks/{task_id}/resume",
            post(resume_task),
        )
        .route("/v1/projects/{project_id}/workers", get(list_workers))
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/heartbeat",
            post(post_heartbeat),
        )
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/status",
            post(post_status),
        )
        .route("/v1/projects/{project_id}/admin/tick", post(admin_tick))
        .layer(TraceLayer::new_for_http())
        .layer(play_cors())
        .with_state(state)
}

/// Local play: UI on :3030 calling orchestrator on :8081 with admin bearer.
fn play_cors() -> CorsLayer {
    let origins = [
        "http://localhost:3030",
        "http://127.0.0.1:3030",
    ]
    .into_iter()
    .filter_map(|o| o.parse::<HeaderValue>().ok())
    .collect::<Vec<_>>();

    CorsLayer::new()
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
        ])
}

