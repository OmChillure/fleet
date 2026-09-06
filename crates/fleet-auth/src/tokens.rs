use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub const ACCESS_COOKIE: &str = "fleet_access";
pub const REFRESH_COOKIE: &str = "fleet_refresh";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    /// Subject = user id
    pub sub: String,
    pub email: String,
    /// Session id (auth_sessions.id)
    pub sid: String,
    pub exp: i64,
    pub iat: i64,
}

pub fn issue_access_token(
    secret: &str,
    user_id: Uuid,
    email: &str,
    session_id: Uuid,
    ttl: Duration,
) -> AppResult<String> {
    let now = Utc::now();
    let exp = now + ChronoDuration::from_std(ttl).unwrap_or(ChronoDuration::minutes(15));
    let claims = AccessClaims {
        sub: user_id.to_string(),
        email: email.to_string(),
        sid: session_id.to_string(),
        exp: exp.timestamp(),
        iat: now.timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(AppError::from)
}

pub fn decode_access_token(secret: &str, token: &str) -> AppResult<AccessClaims> {
    let data = decode::<AccessClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(data.claims)
}

/// Logout-only: signature + `sid` required, `exp` not checked.
/// Do not use for `/me` or other gated routes.
pub(crate) fn decode_access_token_for_logout(secret: &str, token: &str) -> AppResult<AccessClaims> {
    let mut validation = Validation::default();
    validation.validate_exp = false;
    let data = decode::<AccessClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}

/// Opaque refresh token (URL-safe hex). Store only the SHA-256 hash.
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_refresh_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SECRET: &str = "test-secret-at-least-32-characters-long!!";
    const OTHER_SECRET: &str = "other-secret-at-least-32-characters-long!";

    #[test]
    fn issue_and_decode_access_jwt() {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let token = issue_access_token(
            SECRET,
            user_id,
            "user@example.test",
            session_id,
            Duration::from_secs(900),
        )
        .expect("issue");

        let claims = decode_access_token(SECRET, &token).expect("decode");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.email, "user@example.test");
        assert_eq!(claims.sid, session_id.to_string());
    }

    #[test]
    fn wrong_secret_fails() {
        let token = issue_access_token(
            SECRET,
            Uuid::new_v4(),
            "user@example.test",
            Uuid::new_v4(),
            Duration::from_secs(900),
        )
        .expect("issue");

        let err = decode_access_token(OTHER_SECRET, &token).expect_err("wrong secret");
        assert!(
            matches!(err, AppError::Unauthorized(_)),
            "expected Unauthorized, got {err:?}"
        );
    }

    #[test]
    fn logout_decode_accepts_expired_token() {
        let now = Utc::now();
        let session_id = Uuid::new_v4();
        let claims = AccessClaims {
            sub: Uuid::new_v4().to_string(),
            email: "user@example.test".into(),
            sid: session_id.to_string(),
            exp: (now - ChronoDuration::seconds(120)).timestamp(),
            iat: (now - ChronoDuration::seconds(180)).timestamp(),
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .expect("encode expired token");

        decode_access_token(SECRET, &token).expect_err("gated decode must still reject expired");
        let decoded = decode_access_token_for_logout(SECRET, &token)
            .expect("logout decode must accept expired access JWT");
        assert_eq!(decoded.sid, session_id.to_string());
    }

    #[test]
    fn logout_decode_rejects_wrong_secret() {
        let token = issue_access_token(
            SECRET,
            Uuid::new_v4(),
            "user@example.test",
            Uuid::new_v4(),
            Duration::from_secs(900),
        )
        .expect("issue");

        let err = decode_access_token_for_logout(OTHER_SECRET, &token).expect_err("wrong secret");
        assert!(
            matches!(err, AppError::Unauthorized(_)),
            "expected Unauthorized, got {err:?}"
        );
    }
}
