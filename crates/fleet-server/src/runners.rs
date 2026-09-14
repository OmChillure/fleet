//! Fleet Cloud / BYO public cloud / Partners attach. One assignment path.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::worker_hosts::{self as host_repo};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{
    NewWorkerHost, RUNNER_KIND_FLEET_CLOUD, RUNNER_KIND_PUBLIC_CLOUD,
    WORKER_HOST_ISOLATION_SANDBOXED,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;
use crate::worker_hosts::{create_pair_material, created_payload, require_owner};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/runners/cloud/fleet",
            post(http_attach_fleet_cloud),
        )
        .route(
            "/api/workspaces/{workspace_id}/runners/cloud/byo",
            post(http_attach_byo_cloud),
        )
        .route(
            "/api/workspaces/{workspace_id}/runners/partners/offer",
            post(http_partner_offer),
        )
        .route(
            "/api/workspaces/{workspace_id}/runners/partners/offers",
            get(http_list_partner_offers),
        )
        .route(
            "/api/workspaces/{workspace_id}/runners/partners/borrow",
            post(http_partner_borrow),
        )
        .route(
            "/api/workspaces/{workspace_id}/runners/partners/kill",
            post(http_partner_kill),
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
) -> AppResult<fleet_db::User> {
    let user = require_user(&auth_state(state), jar).await?;
    workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok(user)
}

fn partners_enabled() -> bool {
    match std::env::var("FLEET_PARTNERS_ENABLED") {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => true,
    }
}

async fn attach_sandboxed(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    name: &str,
    runner_kind: &str,
) -> AppResult<Value> {
    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, name, Some(user_id));
    new.isolation = WORKER_HOST_ISOLATION_SANDBOXED.to_string();
    new.runner_kind = runner_kind.to_string();
    new.allow_host = false;
    new.allow_container = true;
    let (pair_token, pair_token_hash, pair_expires_at) = create_pair_material();
    let host =
        host_repo::create_pending(&state.db, &new, &pair_token_hash, pair_expires_at).await?;
    Ok(created_payload(&state, &host, &pair_token, pair_expires_at))
}

async fn http_attach_fleet_cloud(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let user = require_owner(&state, &jar, workspace_id).await?;
    let payload = attach_sandboxed(
        &state,
        workspace_id,
        user.id,
        "Fleet Cloud",
        RUNNER_KIND_FLEET_CLOUD,
    )
    .await?;
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
struct ByoCloudBody {
    pub provider: String,
    #[serde(default)]
    pub credentials: Option<Value>,
}

async fn http_attach_byo_cloud(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<ByoCloudBody>,
) -> AppResult<Json<Value>> {
    let user = require_owner(&state, &jar, workspace_id).await?;
    let provider = body.provider.trim().to_ascii_lowercase();
    if !matches!(provider.as_str(), "aws" | "gcp" | "azure") {
        return Err(AppError::bad_request("provider must be aws, gcp, or azure"));
    }
    if body
        .credentials
        .as_ref()
        .and_then(|v| v.as_object())
        .is_none_or(|o| o.is_empty())
    {
        return Err(AppError::bad_request("credentials object is required"));
    }
    let payload = attach_sandboxed(
        &state,
        workspace_id,
        user.id,
        &format!("BYO {provider}"),
        RUNNER_KIND_PUBLIC_CLOUD,
    )
    .await?;
    let host_id = payload
        .get("host")
        .and_then(|h| h.get("id"))
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let raw = serde_json::to_vec(&body.credentials.unwrap_or(json!({}))).unwrap_or_default();
    sqlx::query(
        r#"
        INSERT INTO cloud_credentials (id, workspace_id, provider, ciphertext, host_id)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(&provider)
    .bind(raw)
    .bind(host_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
struct PartnerOfferBody {
    pub host_id: Uuid,
}

async fn http_partner_offer(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PartnerOfferBody>,
) -> AppResult<Json<Value>> {
    if !partners_enabled() {
        return Err(AppError::forbidden("partners pool is killed"));
    }
    let _ = require_owner(&state, &jar, workspace_id).await?;
    let host = host_repo::get(&state.db, body.host_id)
        .await?
        .ok_or_else(|| AppError::not_found("host"))?;
    if host.workspace_id != workspace_id {
        return Err(AppError::not_found("host"));
    }
    if host.isolation != WORKER_HOST_ISOLATION_SANDBOXED {
        return Err(AppError::bad_request(
            "partners may only lend a sandboxed host",
        ));
    }
    sqlx::query(
        r#"
        INSERT INTO partner_offers (id, workspace_id, host_id, verified, enabled)
        VALUES ($1, $2, $3, FALSE, TRUE)
        ON CONFLICT (host_id) DO UPDATE SET enabled = TRUE
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(body.host_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "host_id": body.host_id })))
}

async fn http_list_partner_offers(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    if !partners_enabled() {
        return Ok(Json(json!({ "offers": [], "killed": true })));
    }
    let rows: Vec<(Uuid, Uuid, bool)> = sqlx::query_as(
        r#"
        SELECT id, host_id, verified
        FROM partner_offers
        WHERE enabled AND workspace_id = $1
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let offers: Vec<Value> = rows
        .into_iter()
        .map(|(id, host_id, verified)| {
            json!({
                "id": id,
                "host_id": host_id,
                "verified": verified,
                "tier": if verified { "verified" } else { "open" },
            })
        })
        .collect();
    Ok(Json(json!({ "offers": offers, "killed": false })))
}

#[derive(Debug, Deserialize)]
struct PartnerBorrowBody {
    pub offer_id: Uuid,
    #[serde(default)]
    pub allow_open: bool,
}

async fn http_partner_borrow(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PartnerBorrowBody>,
) -> AppResult<Json<Value>> {
    if !partners_enabled() {
        return Err(AppError::forbidden("partners pool is killed"));
    }
    let _ = require_owner(&state, &jar, workspace_id).await?;
    let row: Option<(Uuid, Uuid, bool, bool)> = sqlx::query_as(
        r#"
        SELECT id, host_id, verified, enabled
        FROM partner_offers
        WHERE id = $1
        "#,
    )
    .bind(body.offer_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let (offer_id, host_id, verified, enabled) = row.ok_or_else(|| AppError::not_found("offer"))?;
    if !enabled {
        return Err(AppError::bad_request("offer is not enabled"));
    }
    if !verified && !body.allow_open {
        return Err(AppError::bad_request(
            "open partner requires explicit allow_open",
        ));
    }
    sqlx::query(
        r#"
        INSERT INTO partner_leases (id, offer_id, borrower_workspace_id, status)
        VALUES ($1, $2, $3, 'active')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(offer_id)
    .bind(workspace_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(
        json!({ "ok": true, "host_id": host_id, "verified": verified }),
    ))
}

async fn http_partner_kill(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let user = require_user(&auth_state(&state), &jar).await?;
    if !user.instance_admin {
        return Err(AppError::forbidden("instance admin required"));
    }
    let _ = workspace_id;
    sqlx::query("UPDATE partner_offers SET enabled = FALSE")
        .execute(&state.db)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    sqlx::query(
        "UPDATE partner_leases SET status = 'killed', ended_at = $1 WHERE status = 'active'",
    )
    .bind(Utc::now())
    .execute(&state.db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "killed": true })))
}
