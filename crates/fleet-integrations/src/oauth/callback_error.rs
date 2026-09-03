//! Typed OAuth callback failures. HTTP status comes from the variant, not
//! from scanning `e.to_string()`.

use axum::http::StatusCode;
use fleet_auth::AppError;

/// Fail-closed OAuth callback / GitHub App manifest outcome.
#[derive(Debug)]
pub struct OAuthCallbackError {
    pub workspace_hint: Option<String>,
    kind: OAuthCallbackErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OAuthCallbackErrorKind {
    Unauthenticated,
    InvalidState(String),
    UserMismatch,
    NotAMember,
    ProviderMismatch,
    NonceReuse,
    Client(String),
    Other(String),
}

impl OAuthCallbackError {
    pub fn unauthenticated(workspace_hint: Option<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::Unauthenticated,
        }
    }

    pub fn invalid_state(workspace_hint: Option<String>, message: impl Into<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::InvalidState(message.into()),
        }
    }

    pub fn user_mismatch(workspace_hint: Option<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::UserMismatch,
        }
    }

    pub fn not_a_member(workspace_hint: Option<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::NotAMember,
        }
    }

    pub fn provider_mismatch(workspace_hint: Option<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::ProviderMismatch,
        }
    }

    pub fn nonce_reuse(workspace_hint: Option<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::NonceReuse,
        }
    }

    pub fn client(workspace_hint: Option<String>, message: impl Into<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::Client(message.into()),
        }
    }

    pub fn other(workspace_hint: Option<String>, message: impl Into<String>) -> Self {
        Self {
            workspace_hint,
            kind: OAuthCallbackErrorKind::Other(message.into()),
        }
    }

    /// Map `require_user` / membership / persist [`AppError`] by variant.
    pub fn from_app_error(err: AppError, workspace_hint: Option<String>) -> Self {
        match err {
            AppError::Unauthorized(msg) => Self::invalid_state(workspace_hint, msg),
            AppError::Forbidden(_) => Self::not_a_member(workspace_hint),
            AppError::BadRequest(msg) => Self::client(workspace_hint, msg),
            AppError::Conflict(_) => Self::nonce_reuse(workspace_hint),
            other => Self::other(workspace_hint, other.to_string()),
        }
    }

    pub fn message(&self) -> &str {
        match &self.kind {
            OAuthCallbackErrorKind::Unauthenticated => "not authenticated",
            OAuthCallbackErrorKind::InvalidState(msg) => msg,
            OAuthCallbackErrorKind::UserMismatch => "OAuth state user mismatch",
            OAuthCallbackErrorKind::NotAMember => "not a member of this workspace",
            OAuthCallbackErrorKind::ProviderMismatch => "OAuth state provider mismatch",
            OAuthCallbackErrorKind::NonceReuse => "OAuth state nonce already used",
            OAuthCallbackErrorKind::Client(msg) | OAuthCallbackErrorKind::Other(msg) => msg,
        }
    }

    /// `None` keeps the existing 200 popup-close HTML.
    pub fn status(&self) -> Option<StatusCode> {
        match self.kind {
            OAuthCallbackErrorKind::Unauthenticated | OAuthCallbackErrorKind::InvalidState(_) => {
                Some(StatusCode::UNAUTHORIZED)
            }
            OAuthCallbackErrorKind::UserMismatch | OAuthCallbackErrorKind::NotAMember => {
                Some(StatusCode::FORBIDDEN)
            }
            OAuthCallbackErrorKind::NonceReuse => Some(StatusCode::CONFLICT),
            OAuthCallbackErrorKind::ProviderMismatch => Some(StatusCode::BAD_REQUEST),
            OAuthCallbackErrorKind::Client(_) | OAuthCallbackErrorKind::Other(_) => None,
        }
    }

    pub fn is_expected(&self) -> bool {
        !matches!(self.kind, OAuthCallbackErrorKind::Other(_))
    }
}

impl std::fmt::Display for OAuthCallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_derived_from_variant_not_message_text() {
        assert_eq!(
            OAuthCallbackError::unauthenticated(None).status(),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            OAuthCallbackError::invalid_state(None, "invalid or expired OAuth state").status(),
            Some(StatusCode::UNAUTHORIZED)
        );
        let user = OAuthCallbackError::user_mismatch(None);
        assert_eq!(user.status(), Some(StatusCode::FORBIDDEN));
        assert_eq!(user.message(), "OAuth state user mismatch");
        assert_eq!(
            OAuthCallbackError::not_a_member(None).status(),
            Some(StatusCode::FORBIDDEN)
        );
        let mismatch = OAuthCallbackError::provider_mismatch(None);
        assert_eq!(mismatch.status(), Some(StatusCode::BAD_REQUEST));
        assert_eq!(mismatch.message(), "OAuth state provider mismatch");
        let reuse = OAuthCallbackError::nonce_reuse(None);
        assert_eq!(reuse.status(), Some(StatusCode::CONFLICT));
        assert_eq!(reuse.message(), "OAuth state nonce already used");
        assert_eq!(
            OAuthCallbackError::client(None, "missing OAuth code").status(),
            None
        );
        assert_eq!(
            OAuthCallbackError::other(None, "failed to encrypt credentials").status(),
            None
        );
    }

    #[test]
    fn app_error_maps_by_variant() {
        let unauth = OAuthCallbackError::from_app_error(
            AppError::unauthorized("invalid or expired OAuth state"),
            None,
        );
        assert_eq!(unauth.status(), Some(StatusCode::UNAUTHORIZED));
        assert_eq!(unauth.message(), "invalid or expired OAuth state");

        let forbidden = OAuthCallbackError::from_app_error(
            AppError::forbidden("not a member of this workspace"),
            None,
        );
        assert_eq!(forbidden.status(), Some(StatusCode::FORBIDDEN));

        let denied =
            OAuthCallbackError::from_app_error(AppError::bad_request("GitHub: Not Found"), None);
        assert_eq!(denied.status(), None);
        assert!(denied.message().contains("GitHub"));
    }
}
