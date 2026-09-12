//! Workspace vendor API keys for Run-in-a-container guests.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{AppError, AppResult};
use fleet_db::repos::llm_keys;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/llm-keys",
            get(http_list).post(http_create),
        )
        .route(
            "/api/workspaces/{workspace_id}/llm-keys/{key_id}/revoke",
            post(http_revoke),
        )
}

async fn require_owner(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<fleet_db::User> {
    let (user, role) = crate::fleet_routes::require_member(state, jar, workspace_id).await?;
    if role != "owner" && role != "admin" {
        return Err(AppError::forbidden("owner or admin required"));
    }
    Ok(user)
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = crate::fleet_routes::require_member(&state, &jar, workspace_id).await?;
    let rows = llm_keys::list_active(&state.db, workspace_id).await?;
    Ok(Json(json!({
        "keys": rows.iter().map(|k| k.as_public()).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
struct CreateKeyBody {
    provider: String,
    secret: String,
}

async fn http_create(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateKeyBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let user = require_owner(&state, &jar, workspace_id).await?;
    let provider = body.provider.trim().to_ascii_lowercase();
    if !matches!(
        provider.as_str(),
        "grok" | "xai" | "claude" | "anthropic"
    ) {
        return Err(AppError::bad_request(
            "provider must be grok, xai, claude, or anthropic",
        ));
    }
    let secret = body.secret.trim();
    if secret.is_empty() || secret.len() > 4096 {
        return Err(AppError::bad_request("secret is required"));
    }
    let plaintext = json!({ "api_key": secret });
    let ciphertext = state
        .integrations
        .credentials_key
        .encrypt(&plaintext)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let row = llm_keys::insert(
        &state.db,
        workspace_id,
        &provider,
        ciphertext,
        Some(user.id),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "key": row.as_public() }))))
}

async fn http_revoke(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, key_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    if !llm_keys::revoke(&state.db, workspace_id, key_id).await? {
        return Err(AppError::not_found("key not found"));
    }
    Ok(Json(json!({ "ok": true })))
}
