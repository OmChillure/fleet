use axum::Json;
use axum::extract::State;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{WorkspacePublic, WorkspaceWithRole};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{CreateWorkspaceRequest, WorkspaceListResponse, WorkspaceResponse};
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

/// Same bound as `POST /workspaces` (`name.len() > 80` is 400).
const MAX_WORKSPACE_NAME_LEN: usize = 80;

fn clamp_workspace_name(mut name: String) -> String {
    if name.len() <= MAX_WORKSPACE_NAME_LEN {
        return name;
    }
    let mut end = MAX_WORKSPACE_NAME_LEN;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name.truncate(end);
    name
}

/// Default personal workspace name: `"{display_name}'s workspace"` or
/// `"{email-local}'s workspace"`, clamped to [`MAX_WORKSPACE_NAME_LEN`].
pub fn default_workspace_name(display_name: Option<&str>, email: &str) -> String {
    let name = if let Some(name) = display_name.map(str::trim).filter(|s| !s.is_empty()) {
        format!("{name}'s workspace")
    } else {
        let local = email
            .split('@')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("user");
        format!("{local}'s workspace")
    };
    clamp_workspace_name(name)
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

async fn first_owned_workspace(
    state: &AuthState,
    user_id: Uuid,
) -> AppResult<Option<WorkspaceWithRole>> {
    let existing = workspace_repo::list_for_user(&state.db, user_id).await?;
    Ok(existing.into_iter().find(|w| w.role == "owner"))
}

/// Signup uses `workspace_slug_for_user`. Recreating a default after leave
/// must not reuse that slug — the left row still occupies `workspaces_slug_uidx`.
/// Recreate slugs are deterministic so concurrent `ensure_default_workspace`
/// callers unique-violate on the same slug and return the first insert.
fn unique_default_workspace_slug(base: &str, attempt: u8) -> String {
    match attempt {
        0 => base.to_string(),
        1 => format!("{base}-recreated"),
        n => format!("{base}-recreated-{n}"),
    }
}

/// Ensure the user has a personal (owned) workspace; create one if needed.
///
/// Invited memberships are extra. Login/refresh must land on a workspace
/// the user owns, not the oldest workspace they were added to.
///
/// After the user leaves their last workspace, this mints a *new* personal
/// workspace. It does not re-join the left row (slug is still taken by it).
pub async fn ensure_default_workspace(
    state: &AuthState,
    user_id: Uuid,
    email: &str,
    display_name: Option<&str>,
) -> AppResult<WorkspaceWithRole> {
    if let Some(owned) = first_owned_workspace(state, user_id).await? {
        return Ok(owned);
    }

    let name = default_workspace_name(display_name, email);
    let email_normalized = email.trim().to_lowercase();
    let base = workspace_slug_for_user(&email_normalized, user_id);
    let now = Utc::now();

    for attempt in 0u8..8 {
        let workspace_id = Uuid::now_v7();
        let slug = unique_default_workspace_slug(&base, attempt);
        match workspace_repo::create_owned(&state.db, workspace_id, user_id, &slug, &name, now)
            .await
        {
            Ok(_) => {
                return workspace_repo::find_for_user(&state.db, workspace_id, user_id)
                    .await?
                    .ok_or_else(|| AppError::internal("workspace created but not found"));
            }
            Err(e) if e.is_unique_violation() => {
                // Lost a create race, or leftover signup slug. Prefer the
                // owned row another request just inserted; otherwise retry
                // with a unique suffix instead of 409.
                if let Some(owned) = first_owned_workspace(state, user_id).await? {
                    return Ok(owned);
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    Err(AppError::internal(
        "could not allocate unique workspace slug",
    ))
}

pub async fn list_workspaces(
    State(state): State<AuthState>,
    jar: CookieJar,
) -> AppResult<Json<WorkspaceListResponse>> {
    let user = require_user(&state, &jar).await?;
    // Always guarantee at least one workspace (same as login).
    let _ = ensure_default_workspace(&state, user.id, &user.email, user.display_name.as_deref())
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
    if name.len() > MAX_WORKSPACE_NAME_LEN {
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

    let ws = workspace_repo::create_owned(&state.db, workspace_id, user.id, &slug, &name, now)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recreate_slug_is_deterministic() {
        let base = "user-abcd1234";
        assert_eq!(unique_default_workspace_slug(base, 0), base);
        assert_eq!(
            unique_default_workspace_slug(base, 1),
            "user-abcd1234-recreated"
        );
        assert_eq!(
            unique_default_workspace_slug(base, 2),
            "user-abcd1234-recreated-2"
        );
        assert_eq!(
            unique_default_workspace_slug(base, 1),
            unique_default_workspace_slug(base, 1)
        );
    }
}
