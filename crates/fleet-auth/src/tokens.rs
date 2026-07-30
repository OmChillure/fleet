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

/// Opaque refresh token (URL-safe hex). Store only the SHA-256 hash.
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_refresh_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
