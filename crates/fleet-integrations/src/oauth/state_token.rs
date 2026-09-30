//! Short-lived signed OAuth `state` (JWT) so the callback can trust workspace + user.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_auth::{AppError, AppResult};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const STATE_TTL_MINUTES: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthStateClaims {
    /// User who started Connect.
    pub sub: String,
    pub workspace_id: String,
    pub provider: String,
    /// User-chosen connection name. Empty keeps the legacy unnamed row.
    #[serde(default)]
    pub display_name: String,
    /// Self-hosted base URL (Gitea). Empty uses the server default.
    #[serde(default)]
    pub instance_url: String,
    pub nonce: String,
    pub exp: i64,
    pub iat: i64,
    /// Custom claim type so access tokens can't be reused as state.
    pub typ: String,
}

pub fn issue_oauth_state(
    secret: &str,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
) -> AppResult<String> {
    issue_oauth_state_named(secret, user_id, workspace_id, provider, None, None)
}

pub fn issue_oauth_state_named(
    secret: &str,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
    display_name: Option<&str>,
    instance_url: Option<&str>,
) -> AppResult<String> {
    let now = Utc::now();
    let exp = now + ChronoDuration::minutes(STATE_TTL_MINUTES);
    let claims = OAuthStateClaims {
        sub: user_id.to_string(),
        workspace_id: workspace_id.to_string(),
        provider: provider.to_string(),
        display_name: display_name.unwrap_or("").trim().to_string(),
        instance_url: instance_url.unwrap_or("").trim().trim_end_matches('/').to_string(),
        nonce: Uuid::now_v7().to_string(),
        exp: exp.timestamp(),
        iat: now.timestamp(),
        typ: "oauth_state".into(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::internal(format!("oauth state encode: {e}")))
}

pub fn verify_oauth_state(secret: &str, token: &str) -> AppResult<OAuthStateClaims> {
    let mut validation = Validation::default();
    validation.validate_exp = true;
    let data = decode::<OAuthStateClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::unauthorized("invalid or expired OAuth state"))?;

    if data.claims.typ != "oauth_state" {
        return Err(AppError::unauthorized("invalid OAuth state type"));
    }
    // Verify is not consume — the callback persists the nonce once. A blank
    // nonce cannot be a single-use key.
    if data.claims.nonce.trim().is_empty() {
        return Err(AppError::unauthorized("invalid OAuth state nonce"));
    }
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_auth::AppError;

    #[test]
    fn issue_and_verify_round_trip() {
        let secret = "oauth-state-test-secret";
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let token = issue_oauth_state(secret, user_id, workspace_id, "github").unwrap();

        let claims = verify_oauth_state(secret, &token).expect("valid token");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.workspace_id, workspace_id.to_string());
        assert_eq!(claims.provider, "github");
        assert_eq!(claims.typ, "oauth_state");
        assert!(!claims.nonce.is_empty());
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn wrong_secret_rejected() {
        let token =
            issue_oauth_state("correct-secret", Uuid::now_v7(), Uuid::now_v7(), "slack").unwrap();

        let err = verify_oauth_state("other-secret", &token).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
    }

    #[test]
    fn expired_oauth_state_is_unauthorized() {
        use jsonwebtoken::{EncodingKey, Header, encode};

        let secret = "oauth-state-test-secret";
        let now = chrono::Utc::now().timestamp();
        let token = encode(
            &Header::default(),
            &OAuthStateClaims {
                sub: Uuid::now_v7().to_string(),
                workspace_id: Uuid::now_v7().to_string(),
                provider: "github".into(),
                display_name: String::new(),
                instance_url: String::new(),
                nonce: "n".into(),
                exp: now - 120,
                iat: now - 180,
                typ: "oauth_state".into(),
            },
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let err = verify_oauth_state(secret, &token).unwrap_err();
        match err {
            AppError::Unauthorized(msg) => {
                assert!(
                    msg.contains("invalid") || msg.contains("expired"),
                    "expected invalid or expired, got {msg}"
                );
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[test]
    fn oauth_state_nonce_is_unique_per_issued_token() {
        let secret = "oauth-state-test-secret";
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let a = issue_oauth_state(secret, user_id, workspace_id, "github").unwrap();
        let b = issue_oauth_state(secret, user_id, workspace_id, "github").unwrap();

        let claims_a = verify_oauth_state(secret, &a).expect("first token");
        let claims_b = verify_oauth_state(secret, &b).expect("second token");
        assert_ne!(
            claims_a.nonce, claims_b.nonce,
            "nonce is the single-use key; two issues must not share one"
        );
    }

    #[test]
    fn issued_oauth_state_exp_is_fifteen_minutes_after_iat() {
        let secret = "oauth-state-test-secret";
        let token = issue_oauth_state(secret, Uuid::now_v7(), Uuid::now_v7(), "github").unwrap();
        let claims = verify_oauth_state(secret, &token).expect("valid token");
        let ttl_secs = claims.exp - claims.iat;
        assert!(
            (ttl_secs - STATE_TTL_MINUTES * 60).abs() <= 1,
            "exp-iat must be {STATE_TTL_MINUTES}m, got {ttl_secs}s"
        );
    }

    #[test]
    fn oauth_state_empty_nonce_is_rejected() {
        use jsonwebtoken::{EncodingKey, Header, encode};

        let secret = "oauth-state-test-secret";
        let now = chrono::Utc::now().timestamp();
        for nonce in ["", "   "] {
            let token = encode(
                &Header::default(),
                &OAuthStateClaims {
                    sub: Uuid::now_v7().to_string(),
                    workspace_id: Uuid::now_v7().to_string(),
                    provider: "github".into(),
                    display_name: String::new(),
                    instance_url: String::new(),
                    nonce: nonce.into(),
                    exp: now + 600,
                    iat: now,
                    typ: "oauth_state".into(),
                },
                &EncodingKey::from_secret(secret.as_bytes()),
            )
            .unwrap();

            let err = verify_oauth_state(secret, &token).unwrap_err();
            match err {
                AppError::Unauthorized(msg) => {
                    assert!(
                        msg.to_ascii_lowercase().contains("nonce")
                            || msg.to_ascii_lowercase().contains("invalid"),
                        "blank nonce {nonce:?} must not verify, got {msg}"
                    );
                }
                other => panic!("expected Unauthorized for blank nonce {nonce:?}, got {other:?}"),
            }
        }
    }
}
