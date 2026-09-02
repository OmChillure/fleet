use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fleet_db::DbError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    TooManyRequests(String),
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::Unauthorized(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self::TooManyRequests(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    fn kind(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "bad_request",
            AppError::Unauthorized(_) => "unauthorized",
            AppError::Forbidden(_) => "forbidden",
            AppError::NotFound(_) => "not_found",
            AppError::Conflict(_) => "conflict",
            AppError::TooManyRequests(_) => "too_many_requests",
            AppError::Internal(_) => "internal",
        }
    }
}

impl From<DbError> for AppError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Conflict(msg) => {
                tracing::warn!(error = %msg, "database conflict");
                Self::Conflict(msg)
            }
            DbError::NotFound(msg) => {
                tracing::debug!(error = %msg, "database not found");
                Self::NotFound(msg)
            }
            DbError::Connect {
                ref target,
                ref source,
            } => {
                // Full detail only in logs — client gets a safe message.
                tracing::error!(
                    db_target = %target,
                    error = %source,
                    "database connect error"
                );
                Self::Internal("database unavailable".into())
            }
            DbError::Sqlx(ref e) => {
                // sqlx Display includes PG code/message when available.
                tracing::error!(error = %e, debug = ?e, "database query error");
                Self::Internal("database error".into())
            }
            DbError::Migrate(ref e) => {
                tracing::error!(error = %e, "database migration error");
                Self::Internal("database migration error".into())
            }
        }
    }
}

impl From<jsonwebtoken::errors::Error> for AppError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        tracing::debug!(error = %err, "jwt decode/validate error");
        Self::Unauthorized("invalid or expired token".into())
    }
}

impl From<argon2::password_hash::Error> for AppError {
    fn from(err: argon2::password_hash::Error) -> Self {
        tracing::error!(error = %err, "password hash error");
        Self::Internal("password hashing failed".into())
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let kind = self.kind();
        let (status, msg) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.clone()),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, m.clone()),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };

        // Central log for every API error so nothing fails silently.
        // 5xx → error, 4xx → warn (both visible under default filters).
        if status.is_server_error() {
            tracing::error!(
                status = status.as_u16(),
                kind,
                error = %msg,
                "api error"
            );
        } else {
            tracing::warn!(
                status = status.as_u16(),
                kind,
                error = %msg,
                "api error"
            );
        }

        (status, Json(ErrorBody { error: msg })).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
