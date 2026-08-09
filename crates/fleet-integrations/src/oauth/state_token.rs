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
    let now = Utc::now();
    let exp = now + ChronoDuration::minutes(STATE_TTL_MINUTES);
    let claims = OAuthStateClaims {
        sub: user_id.to_string(),
        workspace_id: workspace_id.to_string(),
        provider: provider.to_string(),
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
    Ok(data.claims)
}
