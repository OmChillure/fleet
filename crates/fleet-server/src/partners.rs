//! Partners disclaimer ack (timestamp only).
//!
//! Acknowledging does **not** enable lending, borrowing, enqueue, or
//! assignable partner runners.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::PartnersDisclaimerAck;
use fleet_db::repos::workspaces as workspace_repo;
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/workspaces/{workspace_id}/partners/disclaimer",
        get(get_disclaimer).post(post_disclaimer),
    )
}

fn auth_state(state: &AppState) -> AuthState {
    AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    }
}

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let user = require_user(&auth_state(state), jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, role))
}

async fn get_disclaimer(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PartnersDisclaimerAck>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let accepted_at =
        workspace_repo::partners_disclaimer_accepted_at(&state.db, workspace_id).await?;
    Ok(Json(PartnersDisclaimerAck::from_accepted_at(accepted_at)))
}

async fn post_disclaimer(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PartnersDisclaimerAck>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let accepted_at =
        workspace_repo::accept_partners_disclaimer(&state.db, workspace_id, Utc::now()).await?;
    Ok(Json(PartnersDisclaimerAck::from_accepted_at(Some(
        accepted_at,
    ))))
}
