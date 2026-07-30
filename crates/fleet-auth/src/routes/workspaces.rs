use axum::Json;
use axum::extract::State;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{WorkspacePublic, WorkspaceWithRole};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{
    CreateWorkspaceRequest, WorkspaceListResponse, WorkspaceResponse,
};
use crate::routes::auth::require_user;
use crate::state::AuthState;

fn slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "workspace".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

/// Default personal workspace name: `"{email-local}'s workspace"`.
pub fn default_workspace_name(display_name: Option<&str>, email: &str) -> String {
    if let Some(name) = display_name.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("{name}'s workspace");
    }
    let local = email
        .split('@')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("user");
    format!("{local}'s workspace")
}

pub fn workspace_slug_for_user(email_normalized: &str, user_id: Uuid) -> String {
    let base: String = email_normalized
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let base = base.trim_matches('-');
    let base: String = base.chars().take(24).collect();
    let base = if base.is_empty() {
        "user".to_string()
    } else {
        base
    };
    format!("{base}-{}", &user_id.to_string()[..8])
}

/// Ensure the user has at least one workspace; create a default if needed.
pub async fn ensure_default_workspace(
    state: &AuthState,
    user_id: Uuid,
    email: &str,
    display_name: Option<&str>,
) -> AppResult<WorkspaceWithRole> {
    let existing = workspace_repo::list_for_user(&state.db, user_id).await?;
    if let Some(first) = existing.into_iter().next() {
        return Ok(first);
    }

    let workspace_id = Uuid::now_v7();
    let name = default_workspace_name(display_name, email);
    let email_normalized = email.trim().to_lowercase();
    let slug = workspace_slug_for_user(&email_normalized, user_id);
    let now = Utc::now();

    workspace_repo::create_owned(&state.db, workspace_id, user_id, &slug, &name, now).await?;

    workspace_repo::find_for_user(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::internal("workspace created but not found"))
}

pub async fn list_workspaces(
    State(state): State<AuthState>,
    jar: CookieJar,
) -> AppResult<Json<WorkspaceListResponse>> {
    let user = require_user(&state, &jar).await?;
    // Always guarantee at least one workspace (same as login).
    let _ = ensure_default_workspace(
        &state,
        user.id,
        &user.email,
        user.display_name.as_deref(),
    )
    .await?;
    let rows = workspace_repo::list_for_user(&state.db, user.id).await?;
    Ok(Json(WorkspaceListResponse {
        workspaces: rows.into_iter().map(WorkspacePublic::from).collect(),
    }))
}

pub async fn create_workspace(
    State(state): State<AuthState>,
    jar: CookieJar,
    Json(body): Json<CreateWorkspaceRequest>,
) -> AppResult<(axum::http::StatusCode, Json<WorkspaceResponse>)> {
    let user = require_user(&state, &jar).await?;
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("workspace name is required"));
    }
    if name.len() > 80 {
        return Err(AppError::bad_request("workspace name is too long"));
    }

    let workspace_id = Uuid::now_v7();
    let base_slug = body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(&name));
    let slug = format!("{base_slug}-{}", &workspace_id.to_string()[..8]);
    let now = Utc::now();

    let ws = workspace_repo::create_owned(
        &state.db,
        workspace_id,
        user.id,
        &slug,
        &name,
        now,
    )
    .await
    .map_err(|e| {
        if e.is_unique_violation() {
            AppError::conflict("workspace slug already exists")
        } else {
            AppError::from(e)
        }
    })?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(WorkspaceResponse {
            workspace: WorkspacePublic {
                id: ws.id,
                slug: ws.slug,
                name: ws.name,
                role: "owner".into(),
            },
        }),
    ))
}
