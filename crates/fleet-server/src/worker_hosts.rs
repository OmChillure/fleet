//! Cookie + machine HTTP for paired worker hosts.
//!
//! Cookie: workspace owner mutates; any member can GET list/get.
//! Machine: pair token once, then `Authorization: Bearer flm_…`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::{Duration, Utc};
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::repos::plane_runners;
use fleet_db::repos::work_claims;
use fleet_db::repos::worker_hosts::{self as host_repo, PairedWorkerHost};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{
    HostCpSettings, NewWorkerHost, WorkerHost, WorkerHostPublic, is_control_plane_runner_id,
    parse_worker_host_isolation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

const PAIR_TTL: Duration = Duration::hours(1);
const LEASE_FOR: Duration = Duration::minutes(2);
const ROLE_OWNER: &str = "owner";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/worker-hosts",
            get(http_list).post(http_create),
        )
        .route(
            "/api/workspaces/{workspace_id}/worker-hosts/{host_id}",
            get(http_get).patch(http_patch).delete(http_delete),
        )
        .route(
            "/api/workspaces/{workspace_id}/worker-hosts/{host_id}/pair-tokens",
            post(http_new_pair_token),
        )
        .route("/api/worker-hosts/pair", post(http_pair))
        .route("/api/worker-hosts/me/heartbeat", post(http_heartbeat))
        .route("/api/worker-hosts/me/lease", post(http_lease))
        .route(
            "/api/worker-hosts/me/lease/{claim_id}/complete",
            post(http_complete),
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

async fn require_owner(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<fleet_db::User> {
    let (user, role) = require_member(state, jar, workspace_id).await?;
    if role != ROLE_OWNER {
        return Err(AppError::forbidden("workspace owner required"));
    }
    Ok(user)
}

fn parse_machine_id(raw: Option<&str>) -> Option<Uuid> {
    Uuid::parse_str(raw?.trim()).ok()
}

fn desired_for(state: &AppState, host: &WorkerHost) -> Option<fleet_db::WorkerSoftwareManifest> {
    crate::install_routes::load_platform_manifest(state, host.platform.as_deref())
}

fn host_json(state: &AppState, host: &WorkerHost) -> WorkerHostPublic {
    host.as_public(Utc::now())
        .with_desired(desired_for(state, host))
}

fn plane_git_sha() -> Option<&'static str> {
    option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

async fn plane_public(state: &AppState, workspace_id: Uuid) -> AppResult<WorkerHostPublic> {
    let row = plane_runners::get_or_default(&state.db, workspace_id).await?;
    let jobs = state
        .orch
        .store
        .count_local_capacity(workspace_id)
        .await
        .unwrap_or(0);
    Ok(row.as_public_with_jobs(Utc::now(), jobs))
}

fn api_base(state: &AppState) -> String {
    state
        .integrations
        .api_public_base
        .trim_end_matches('/')
        .to_string()
}

fn install_hint(api: &str, pair_token: &str) -> String {
    format!("curl -fsSL {api}/install-worker.sh | FLEET_URL={api} FLEET_PAIR_TOKEN={pair_token} sh")
}

fn cp_settings_json(host: &WorkerHost) -> Value {
    json!({
        "name": host.name,
        "enabled": host.enabled,
        "paused": host.paused,
        "drain": host.drain,
        "isolation": host.isolation,
        "allow_host": host.allow_host,
        "allow_container": host.allow_container,
        "max_concurrent": host.max_concurrent,
        "max_job_timeout_s": host.max_job_timeout_s,
        "labels": host.labels,
        "settings_gen": host.settings_gen,
    })
}

fn effective_json(host: &WorkerHost) -> Value {
    json!({
        "host": host.effective_host(),
        "container": host.effective_container(),
        "max_concurrent": host.max_concurrent,
    })
}

fn create_pair_material() -> (String, String, chrono::DateTime<Utc>) {
    let pair_token = host_repo::generate_pair_token();
    let pair_token_hash = host_repo::hash_token(&pair_token);
    let pair_expires_at = Utc::now() + PAIR_TTL;
    (pair_token, pair_token_hash, pair_expires_at)
}

fn created_payload(
    state: &AppState,
    host: &WorkerHost,
    pair_token: &str,
    pair_expires_at: chrono::DateTime<Utc>,
) -> Value {
    let api = api_base(state);
    json!({
        "host": host_json(state, host),
        "pair_token": pair_token,
        "pair_expires_at": pair_expires_at,
        "install_hint": install_hint(&api, pair_token),
    })
}

fn validate_max_concurrent(n: i32) -> AppResult<()> {
    if !(1..=256).contains(&n) {
        return Err(AppError::bad_request(
            "max_concurrent must be between 1 and 256",
        ));
    }
    Ok(())
}

fn validate_max_job_timeout_s(n: i32) -> AppResult<()> {
    if n < 60 {
        return Err(AppError::bad_request(
            "max_job_timeout_s must be at least 60",
        ));
    }
    Ok(())
}

fn validate_isolation(raw: &str) -> AppResult<&'static str> {
    parse_worker_host_isolation(raw)
        .ok_or_else(|| AppError::bad_request("isolation must be native or sandboxed"))
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkerHostBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub allow_host: Option<bool>,
    #[serde(default)]
    pub allow_container: Option<bool>,
    #[serde(default)]
    pub max_concurrent: Option<i32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PatchWorkerHostBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub paused: Option<bool>,
    #[serde(default)]
    pub drain: Option<bool>,
    #[serde(default)]
    pub isolation: Option<String>,
    #[serde(default)]
    pub allow_host: Option<bool>,
    #[serde(default)]
    pub allow_container: Option<bool>,
    #[serde(default)]
    pub max_concurrent: Option<i32>,
    #[serde(default)]
    pub max_job_timeout_s: Option<i32>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub auto_update: Option<bool>,
    #[serde(default)]
    pub request_update: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PairBody {
    pub token: String,
    pub hostname: String,
    pub platform: String,
    pub advertised_host: bool,
    pub advertised_container: bool,
    #[serde(default)]
    pub machine_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatBody {
    pub advertised_host: bool,
    pub advertised_container: bool,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    pub current_jobs: i32,
    /// Host-local cap; plane stores it as advertised_max_concurrent.
    #[serde(default)]
    pub max_concurrent: Option<i32>,
    #[serde(default)]
    pub error: Option<String>,
    /// Durable box id. Same file on the machine after remove + re-pair.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Optional; omitted by hosts built before version reporting.
    #[serde(default)]
    pub software: Option<HeartbeatSoftware>,
    /// Optional machine snapshot (cores / RAM / OS).
    #[serde(default)]
    pub hardware: Option<serde_json::Value>,
    /// Last `settings_gen` this box applied from a heartbeat response.
    #[serde(default)]
    pub settings_ack_gen: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatSoftware {
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub bins: Option<serde_json::Value>,
    #[serde(default)]
    pub updating: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteBody {
    pub lease_token: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

async fn http_create(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateWorkerHostBody>,
) -> AppResult<Json<Value>> {
    let user = require_owner(&state, &jar, workspace_id).await?;
    if let Some(n) = body.max_concurrent {
        validate_max_concurrent(n)?;
    }
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, name, Some(user.id));
    if let Some(v) = body.allow_host {
        new.allow_host = v;
    }
    if let Some(v) = body.allow_container {
        new.allow_container = v;
    }
    if let Some(n) = body.max_concurrent {
        new.max_concurrent = n;
    }

    let (pair_token, pair_token_hash, pair_expires_at) = create_pair_material();
    let host =
        host_repo::create_pending(&state.db, &new, &pair_token_hash, pair_expires_at).await?;
    Ok(Json(created_payload(
        &state,
        &host,
        &pair_token,
        pair_expires_at,
    )))
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let rows = host_repo::list_for_workspace(&state.db, workspace_id).await?;
    let plane = plane_public(&state, workspace_id).await?;
    let mut hosts = Vec::with_capacity(rows.len() + 1);
    hosts.push(plane);
    hosts.extend(rows.iter().map(|h| host_json(&state, h)));
    let packages = crate::install_routes::list_dist_packages(&state);
    let sandbox_backend = std::env::var("SANDBOX_BACKEND")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "stub".into());
    Ok(Json(json!({
        "hosts": hosts,
        "packages": packages,
        "plane_git_sha": plane_git_sha(),
        "sandbox_backend": sandbox_backend,
    })))
}

async fn load_in_workspace(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
) -> AppResult<WorkerHost> {
    host_repo::get_in_workspace(&state.db, workspace_id, host_id)
        .await?
        .ok_or_else(|| AppError::not_found("worker host not found"))
}

async fn http_get(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WorkerHostPublic>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    if is_control_plane_runner_id(workspace_id, host_id) {
        return Ok(Json(plane_public(&state, workspace_id).await?));
    }
    let host = load_in_workspace(&state, workspace_id, host_id).await?;
    Ok(Json(host_json(&state, &host)))
}

async fn http_patch(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchWorkerHostBody>,
) -> AppResult<Json<WorkerHostPublic>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    if is_control_plane_runner_id(workspace_id, host_id) {
        if let Some(n) = body.max_concurrent {
            validate_max_concurrent(n)?;
        }
        // TODO: pause/drain/auto_update/request_update/isolation column are
        // paired-host settings. Plane uses allow_host/allow_container only.
        let row = plane_runners::upsert(
            &state.db,
            workspace_id,
            &plane_runners::PlaneRunnerPatch {
                name: body
                    .name
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                allow_host: body.allow_host.or(body.enabled).or(body.paused.map(|p| !p)),
                allow_container: body.allow_container,
                max_concurrent: body.max_concurrent,
            },
        )
        .await?;
        let jobs = state
            .orch
            .store
            .count_local_capacity(workspace_id)
            .await
            .unwrap_or(0);
        return Ok(Json(row.as_public_with_jobs(Utc::now(), jobs)));
    }
    let _ = load_in_workspace(&state, workspace_id, host_id).await?;
    if let Some(n) = body.max_concurrent {
        validate_max_concurrent(n)?;
    }
    if let Some(n) = body.max_job_timeout_s {
        validate_max_job_timeout_s(n)?;
    }
    let isolation = match body.isolation.as_deref() {
        None => None,
        Some(raw) => Some(validate_isolation(raw)?.to_string()),
    };
    let patch = HostCpSettings {
        name: body
            .name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        enabled: body.enabled,
        paused: body.paused,
        drain: body.drain,
        isolation,
        allow_host: body.allow_host,
        allow_container: body.allow_container,
        max_concurrent: body.max_concurrent,
        max_job_timeout_s: body.max_job_timeout_s,
        labels: body.labels,
        auto_update: body.auto_update,
        request_update: body.request_update,
    };
    let host = host_repo::patch_cp_settings(&state.db, host_id, &patch).await?;
    Ok(Json(host_json(&state, &host)))
}

async fn http_delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WorkerHostPublic>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    if is_control_plane_runner_id(workspace_id, host_id) {
        return Err(AppError::bad_request(
            "the control plane runner cannot be removed",
        ));
    }
    let _ = load_in_workspace(&state, workspace_id, host_id).await?;
    let host = host_repo::revoke(&state.db, host_id).await?;
    Ok(Json(host_json(&state, &host)))
}

async fn http_new_pair_token(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    if is_control_plane_runner_id(workspace_id, host_id) {
        return Err(AppError::bad_request(
            "the control plane runner does not pair",
        ));
    }
    let _ = load_in_workspace(&state, workspace_id, host_id).await?;
    let (pair_token, pair_token_hash, pair_expires_at) = create_pair_material();
    let host = host_repo::replace_pair_token(&state.db, host_id, &pair_token_hash, pair_expires_at)
        .await?;
    Ok(Json(created_payload(
        &state,
        &host,
        &pair_token,
        pair_expires_at,
    )))
}

fn map_pair_err(err: fleet_db::DbError) -> AppError {
    match err {
        fleet_db::DbError::NotFound(_) => AppError::unauthorized("invalid or expired pair token"),
        fleet_db::DbError::Conflict(msg) => AppError::conflict(msg),
        other => AppError::from(other),
    }
}

async fn http_pair(
    State(state): State<AppState>,
    Json(body): Json<PairBody>,
) -> AppResult<Json<Value>> {
    let token = body.token.trim();
    if token.is_empty() || !token.starts_with(host_repo::PAIR_TOKEN_PREFIX) {
        return Err(AppError::unauthorized("invalid or expired pair token"));
    }
    let hostname = body.hostname.trim();
    let platform = body.platform.trim();
    if hostname.is_empty() {
        return Err(AppError::bad_request("hostname is required"));
    }
    if platform.is_empty() {
        return Err(AppError::bad_request("platform is required"));
    }
    let PairedWorkerHost {
        host,
        machine_token,
    } = host_repo::consume_pair_token(
        &state.db,
        token,
        hostname,
        platform,
        body.advertised_host,
        body.advertised_container,
        parse_machine_id(body.machine_id.as_deref()),
    )
    .await
    .map_err(map_pair_err)?;
    Ok(Json(json!({
        "host_id": host.id,
        "machine_token": machine_token,
        "workspace_id": host.workspace_id,
        "control_plane_url": api_base(&state),
        "cp_settings": cp_settings_json(&host),
    })))
}

fn bearer_token(headers: &HeaderMap) -> AppResult<&str> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::unauthorized("not authenticated"))?;
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::unauthorized("not authenticated"))
}

async fn require_machine(state: &AppState, headers: &HeaderMap) -> AppResult<WorkerHost> {
    let token = bearer_token(headers)?;
    if !token.starts_with(host_repo::MACHINE_TOKEN_PREFIX) {
        return Err(AppError::unauthorized("invalid machine token"));
    }
    host_repo::authenticate_machine(&state.db, token)
        .await?
        .ok_or_else(|| AppError::unauthorized("invalid machine token"))
}

async fn http_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HeartbeatBody>,
) -> AppResult<Json<Value>> {
    let host = require_machine(&state, &headers).await?;
    let platform = body
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let hostname = body
        .hostname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let last_error = body
        .error
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let sw = body.software.as_ref();
    let host = host_repo::heartbeat_with_machine(
        &state.db,
        host.id,
        body.advertised_host,
        body.advertised_container,
        platform,
        hostname,
        last_error,
        body.current_jobs,
        body.max_concurrent,
        sw.and_then(|s| s.git_sha.as_deref()),
        sw.and_then(|s| s.bins.clone()),
        sw.and_then(|s| s.updating),
        body.hardware.clone(),
        parse_machine_id(body.machine_id.as_deref()),
    )
    .await?;
    let host = if let Some(n) = body.settings_ack_gen {
        host_repo::ack_settings(&state.db, host.id, n)
            .await
            .unwrap_or(host)
    } else {
        host
    };
    let public = host_json(&state, &host);
    let desired = public.desired.clone();
    Ok(Json(json!({
        "host": public,
        "effective": effective_json(&host),
        "desired": desired,
    })))
}

fn host_accepts_leases(host: &WorkerHost) -> bool {
    host.enabled
        && !host.paused
        && !host.drain
        && host.is_native_isolation()
        && host.status == fleet_db::WORKER_HOST_STATUS_ONLINE
        && host.effective_host()
}

async fn http_lease(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let host = require_machine(&state, &headers).await?;
    if !host_accepts_leases(&host) {
        return Ok(Json(json!({ "claim": null })));
    }
    let claim =
        work_claims::claim_next_spawn_for_host(&state.db, host.workspace_id, host.id, LEASE_FOR)
            .await?;
    match claim {
        None => Ok(Json(json!({ "claim": null }))),
        Some(c) => Ok(Json(json!({
            "claim": {
                "id": c.id,
                "lease_token": c.lease_token,
                "payload": c.payload,
            }
        }))),
    }
}

async fn http_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id): Path<Uuid>,
    Json(body): Json<CompleteBody>,
) -> AppResult<Json<Value>> {
    let host = require_machine(&state, &headers).await?;
    let lease_token = Uuid::parse_str(body.lease_token.trim())
        .map_err(|_| AppError::bad_request("lease_token must be a UUID"))?;
    let claim = work_claims::get(&state.db, claim_id)
        .await?
        .ok_or_else(|| AppError::not_found("claim not found"))?;
    // Always consume the lease. Re-queueing the same session_id while the
    // worker_sessions row is still live would double-spawn.
    let held =
        work_claims::complete_owned(&state.db, claim_id, lease_token, host.id, host.workspace_id)
            .await?;
    if !held {
        return Err(AppError::conflict("lease not held or already finished"));
    }
    if !body.ok {
        if let Some(sid) = claim
            .payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            let sess = state
                .orch
                .store
                .get_session(sid)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
            let attempt = sess.as_ref().map(|s| s.attempt).unwrap_or(1);
            let tid = claim
                .payload
                .get("board_task_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .or_else(|| sess.as_ref().map(|s| s.board_task_id));
            let max_retries = if let (Some(s), Some(task_id)) = (sess.as_ref(), tid) {
                state
                    .orch
                    .store
                    .get_task(s.project_id, task_id)
                    .await
                    .map_err(|e| AppError::internal(e.to_string()))?
                    .map(|t| t.max_retries)
                    .unwrap_or(2)
            } else {
                2
            };
            state
                .orch
                .store
                .complete_spawn_failure(sid)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
            if let Some(tid) = tid {
                fleet_orchestrator::orch::apply_retry_after_failure(
                    &state.orch.store,
                    tid,
                    attempt,
                    max_retries,
                    false,
                    fleet_orchestrator::domain::ExitReason::SpawnFailed,
                )
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}
