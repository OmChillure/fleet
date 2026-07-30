mod auth;
mod workspaces;

use axum::Router;
use axum::extract::FromRef;
use axum::routing::{get, post};

use crate::state::AuthState;

/// Auth + workspace HTTP routes. `S` is the host app state (`fleet-server::AppState`);
/// handlers extract [`AuthState`] via [`FromRef`].
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AuthState: FromRef<S>,
{
    Router::new()
        .route("/api/auth/signup", post(auth::signup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/refresh", post(auth::refresh))
        .route("/api/auth/me", get(auth::me))
        .route(
            "/api/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
}
