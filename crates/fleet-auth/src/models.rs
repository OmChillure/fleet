//! HTTP request/response DTOs for the auth API.
//! Row types live in `fleet-db`.

use serde::{Deserialize, Serialize};

use fleet_db::UserPublic;

#[derive(Debug, Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub user: UserPublic,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: UserPublic,
}
