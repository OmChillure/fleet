use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;

use super::state::AppState;
use crate::config::AuthMode;

pub struct AdminAuth;

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.auth_mode == AuthMode::None {
            return Ok(AdminAuth);
        }
        // No ADMIN_TOKEN configured → /v1 admin automation is disabled.
        // Use cookie-authenticated /api/workspaces/{id}/fleet/* instead.
        if state.admin_token.trim().is_empty() {
            return Err((
                StatusCode::UNAUTHORIZED,
                "admin automation disabled (set ADMIN_TOKEN to enable /v1 bearer routes)".into(),
            ));
        }
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .unwrap_or("");
        if token.is_empty() || token != state.admin_token {
            return Err((StatusCode::UNAUTHORIZED, "invalid admin token".into()));
        }
        Ok(AdminAuth)
    }
}

pub struct WorkerAuth {
    pub token: String,
}

impl FromRequestParts<AppState> for WorkerAuth {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .unwrap_or("")
            .to_string();
        if token.is_empty() {
            return Err((StatusCode::UNAUTHORIZED, "worker token required".into()));
        }
        Ok(WorkerAuth { token })
    }
}
