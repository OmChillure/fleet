//! Workspace members + email invites.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch};
use axum::Router;
use axum::extract::FromRef;
use axum_extra::extract::CookieJar;
use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::repos::{users as user_repo, workspaces as workspace_repo};
use fleet_db::{WorkspaceInvite, WorkspaceMemberPublic};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::routes::auth::require_user;
use crate::state::AuthState;

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AuthState: FromRef<S>,
{
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/members",
            get(list_members),
        )
        .route(
            "/api/workspaces/{workspace_id}/members/{user_id}",
            patch(patch_member).delete(delete_member),
        )
        .route(
            "/api/workspaces/{workspace_id}/invites",
            get(list_invites).post(create_invite),
        )
        .route(
            "/api/workspaces/{workspace_id}/invites/{invite_id}",
            delete(revoke_invite),
        )
}

fn can_manage(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

async fn require_member(
    state: &AuthState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let user = require_user(state, jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, role))
}

#[derive(Debug, Serialize)]
struct MemberListResponse {
    members: Vec<MemberJson>,
    you: MemberYou,
}

#[derive(Debug, Serialize)]
struct MemberJson {
    user_id: Uuid,
    email: String,
    display_name: Option<String>,
    role: String,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct MemberYou {
    user_id: Uuid,
    role: String,
}

impl From<&WorkspaceMemberPublic> for MemberJson {
    fn from(m: &WorkspaceMemberPublic) -> Self {
        Self {
            user_id: m.user_id,
            email: m.email.clone(),
            display_name: m.display_name.clone(),
            role: m.role.clone(),
            created_at: m.created_at,
        }
    }
}

async fn list_members(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<MemberListResponse>> {
    let (user, role) = require_member(&state, &jar, workspace_id).await?;
    let rows = workspace_repo::list_members(&state.db, workspace_id).await?;
    Ok(Json(MemberListResponse {
        members: rows.iter().map(MemberJson::from).collect(),
        you: MemberYou {
            user_id: user.id,
            role,
        },
    }))
}

#[derive(Debug, Deserialize)]
struct PatchMemberBody {
    role: String,
}

async fn patch_member(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchMemberBody>,
) -> AppResult<Json<MemberJson>> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    if actor_role != "owner" {
        return Err(AppError::forbidden("only an owner can change roles"));
    }
    let new_role = workspace_repo::normalize_member_role(&body.role)
        .ok_or_else(|| AppError::bad_request("role must be owner, admin, or member"))?;
    if user_id == actor.id && new_role != "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot demote the last owner"));
        }
    }
    let current = workspace_repo::user_role(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("member not found"))?;
    if current == "owner" && new_role != "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot demote the last owner"));
        }
    }
    let ok = workspace_repo::set_member_role(&state.db, workspace_id, user_id, new_role).await?;
    if !ok {
        return Err(AppError::not_found("member not found"));
    }
    let rows = workspace_repo::list_members(&state.db, workspace_id).await?;
    let m = rows
        .iter()
        .find(|r| r.user_id == user_id)
        .ok_or_else(|| AppError::not_found("member not found"))?;
    Ok(Json(MemberJson::from(m)))
}

async fn delete_member(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    let leaving = actor.id == user_id;
    if !leaving && !can_manage(&actor_role) {
        return Err(AppError::forbidden("not allowed to remove members"));
    }
    if !leaving && actor_role == "admin" {
        let target = workspace_repo::user_role(&state.db, workspace_id, user_id)
            .await?
            .ok_or_else(|| AppError::not_found("member not found"))?;
        if target == "owner" {
            return Err(AppError::forbidden("admins cannot remove an owner"));
        }
    }
    let target = workspace_repo::user_role(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("member not found"))?;
    if target == "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot remove the last owner"));
        }
    }
    let ok = workspace_repo::remove_member(&state.db, workspace_id, user_id).await?;
    if !ok {
        return Err(AppError::not_found("member not found"));
    }
    Ok(Json(serde_json::json!({ "removed": true, "user_id": user_id })))
}

#[derive(Debug, Serialize)]
struct InviteJson {
    id: Uuid,
    email: String,
    role: String,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
}

impl From<&WorkspaceInvite> for InviteJson {
    fn from(i: &WorkspaceInvite) -> Self {
        Self {
            id: i.id,
            email: i.email_normalized.clone(),
            role: i.role.clone(),
            created_at: i.created_at,
            expires_at: i.expires_at,
        }
    }
}

#[derive(Debug, Serialize)]
struct InviteListResponse {
    invites: Vec<InviteJson>,
}

async fn list_invites(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<InviteListResponse>> {
    let (_, role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&role) {
        return Err(AppError::forbidden("not allowed to list invites"));
    }
    let rows = workspace_repo::list_open_invites(&state.db, workspace_id).await?;
    Ok(Json(InviteListResponse {
        invites: rows.iter().map(InviteJson::from).collect(),
    }))
}

#[derive(Debug, Deserialize)]
struct CreateInviteBody {
    email: String,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateInviteResponse {
    invite: InviteJson,
    added_existing_user: bool,
}

async fn create_invite(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateInviteBody>,
) -> AppResult<(StatusCode, Json<CreateInviteResponse>)> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&actor_role) {
        return Err(AppError::forbidden("not allowed to invite"));
    }
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(AppError::bad_request("valid email is required"));
    }
    let role = workspace_repo::normalize_invite_role(body.role.as_deref().unwrap_or("member"))
        .ok_or_else(|| AppError::bad_request("invite role must be admin or member"))?;
    if actor_role == "admin" && role == "admin" {
        return Err(AppError::forbidden("admins can only invite members"));
    }

    if let Some(existing) = user_repo::find_by_email_normalized(&state.db, &email).await? {
        if workspace_repo::user_role(&state.db, workspace_id, existing.id)
            .await?
            .is_some()
        {
            return Err(AppError::conflict("user is already a member"));
        }
        let now = Utc::now();
        workspace_repo::add_member(&state.db, workspace_id, existing.id, role, now).await?;
        // Synthetic invite row so the UI can show “added”.
        return Ok((
            StatusCode::CREATED,
            Json(CreateInviteResponse {
                invite: InviteJson {
                    id: existing.id,
                    email,
                    role: role.to_string(),
                    created_at: now,
                    expires_at: now,
                },
                added_existing_user: true,
            }),
        ));
    }

    let now = Utc::now();
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let invite = WorkspaceInvite {
        id: Uuid::now_v7(),
        workspace_id,
        email_normalized: email,
        role: role.to_string(),
        token,
        invited_by: actor.id,
        created_at: now,
        expires_at: now + ChronoDuration::days(14),
        accepted_at: None,
        accepted_user_id: None,
        revoked_at: None,
    };
    let saved = workspace_repo::insert_invite(&state.db, &invite)
        .await
        .map_err(|e| {
            if e.is_unique_violation() {
                AppError::conflict("an open invite already exists for that email")
            } else {
                AppError::from(e)
            }
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            invite: InviteJson::from(&saved),
            added_existing_user: false,
        }),
    ))
}

async fn revoke_invite(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, invite_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    let (_, role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&role) {
        return Err(AppError::forbidden("not allowed to revoke invites"));
    }
    let ok =
        workspace_repo::revoke_invite(&state.db, workspace_id, invite_id, Utc::now()).await?;
    if !ok {
        return Err(AppError::not_found("invite not found"));
    }
    Ok(Json(serde_json::json!({ "revoked": true, "id": invite_id })))
}
