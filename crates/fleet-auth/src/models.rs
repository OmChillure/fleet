//! HTTP request/response DTOs for the auth API.
//! Row types live in `fleet-db`.

use serde::{Deserialize, Serialize};

use fleet_db::{UserPublic, WorkspacePublic};

#[derive(Debug, Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
    /// Required when the server has `SIGNUP_TOKEN` set.
    pub token: Option<String>,
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
    /// First / default workspace so the UI can open `/{id}/dashboard` after login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<WorkspacePublic>,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: UserPublic,
    pub workspaces: Vec<WorkspacePublic>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    /// Optional; auto-derived from name when omitted.
    pub slug: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceListResponse {
    pub workspaces: Vec<WorkspacePublic>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub workspace: WorkspacePublic,
}
