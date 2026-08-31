//! Cookie + machine HTTP for paired worker hosts.
//!
//! Cookie: workspace owner mutates; any member can GET list/get.
//! Machine: pair token once, then `Authorization: Bearer flm_…`.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::{Duration, Utc};
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::plane_runners;
use fleet_db::repos::work_claims;
use fleet_db::repos::worker_hosts::{self as host_repo, PairedWorkerHost};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{
    is_control_plane_runner_id, parse_worker_host_isolation, HostCpSettings, NewWorkerHost,
    WorkerHost, WorkerHostPublic,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

fn sanitize_model_details(v: Option<&Value>) -> Value {
    let Some(arr) = v.and_then(|x| x.as_array()) else {
        return json!([]);
    };
    json!(arr
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let efforts: Vec<Value> = obj
                .get("efforts")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            let value = e
                                .get("value")
                                .or_else(|| e.get("id"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .trim();
                            if value.is_empty() {
                                return None;
                            }
                            Some(json!({
                                "value": value,
                                "label": e.get("label").and_then(|l| l.as_str()),
                            }))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(json!({
                "name": name,
                "efforts": efforts,
                "default_effort": obj.get("default_effort").and_then(|d| d.as_str()),
            }))
        })
        .collect::<Vec<_>>())
}

fn sanitize_heartbeat_agents(v: Value) -> Value {
    let Some(arr) = v.as_array() else {
        return json!([]);
    };
    json!(arr
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = obj.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let models: Vec<String> = obj
                .get("models")
                .and_then(|m| m.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let model_details = sanitize_model_details(obj.get("model_details"));
            Some(json!({
                "id": id,
                "name": obj.get("name").and_then(|n| n.as_str()).unwrap_or(id),
                "installed": obj.get("installed").and_then(|b| b.as_bool()).unwrap_or(false),
                "logged_in": obj.get("logged_in").and_then(|b| b.as_bool()).unwrap_or(false),
                "models": models,
                "model_details": model_details,
            }))
        })
        .collect::<Vec<_>>())
}

const LEASE_FOR: Duration = Duration::minutes(2);
/// Sweep size when `/me/lease` returns expired rows to `ready`.
const RECLAIM_EXPIRED_LIMIT: i64 = 64;
const ROLE_OWNER: &str = "owner";
pub const BOOTSTRAP_SECRET_ENV: &str = "FLEET_BOOTSTRAP_SECRET";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/local-models",
            get(http_local_models),
        )
        .route(
            "/api/workspaces/{workspace_id}/local-models/{name}/tags",
            get(http_local_model_tags),
        )
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
        .route("/api/worker-hosts/bootstrap", post(http_bootstrap))
        .route("/api/worker-hosts/me/heartbeat", post(http_heartbeat))
        .route("/api/worker-hosts/me/lease", post(http_lease))
        .route(
            "/api/worker-hosts/me/lease/{claim_id}/complete",
            post(http_complete),
        )
}

async fn http_local_models(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<crate::local_models::LocalModelCatalog>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    Ok(Json(crate::local_models::catalog().await))
}

async fn http_local_model_tags(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, name)): Path<(Uuid, String)>,
) -> AppResult<Json<crate::local_models::LocalModelTags>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    if !crate::local_models::library_slug_ok(&name) {
        return Err(AppError::bad_request(
            "model name must be a library slug (no path or punctuation)".to_string(),
        ));
    }
    Ok(Json(crate::local_models::tags_for(&name).await))
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
    let staged = crate::install_routes::load_platform_manifest(state, host.platform.as_deref())?;
    // Cargo-run plane box already on this package version: do not hand it
    // the staged tarball (different artifact, same number). When it is
    // behind, send desired so auto-update can apply — withholding it is
    // why 5700g stayed behind with auto_update on.
    if should_withhold_desired(host, &staged, &local_hostname()) {
        return None;
    }
    Some(staged)
}

/// This-machine skip only when both bins already report the staged versions.
fn should_withhold_desired(
    host: &WorkerHost,
    staged: &fleet_db::WorkerSoftwareManifest,
    local: &str,
) -> bool {
    host_is_this_machine(host, local)
        && host_on_manifest_versions(host.software_bins.as_ref(), staged)
}

fn host_on_manifest_versions(
    bins: Option<&serde_json::Value>,
    staged: &fleet_db::WorkerSoftwareManifest,
) -> bool {
    let Some(bins) = bins else {
        return false;
    };
    for stem in ["fleet-host", "fleet-driver"] {
        let got = reported_pkg_version(bins, stem).unwrap_or_default();
        let want = staged_pkg_version(staged, stem).unwrap_or_default();
        if got.is_empty() || want.is_empty() || !got.eq_ignore_ascii_case(&want) {
            return false;
        }
    }
    true
}

fn reported_pkg_version(bins: &serde_json::Value, stem: &str) -> Option<String> {
    let obj = bins.as_object()?;
    let exe = format!("{stem}.exe");
    let v = obj.get(stem).or_else(|| obj.get(exe.as_str()))?;
    let s = v.get("version")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn staged_pkg_version(staged: &fleet_db::WorkerSoftwareManifest, stem: &str) -> Option<String> {
    let exe = format!("{stem}.exe");
    staged
        .bins
        .get(stem)
        .or_else(|| staged.bins.get(exe.as_str()))
        .and_then(|b| b.version.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn host_reports_plane_bake(host: &WorkerHost) -> bool {
    sha_matches_plane(host.software_git_sha.as_deref(), plane_git_sha())
}

/// True when the host already runs this process's bake. Those boxes are
/// current and must not be handed the last staged dist (often older).
fn sha_matches_plane(got: Option<&str>, plane: Option<&str>) -> bool {
    match (
        got.map(str::trim).filter(|s| !s.is_empty()),
        plane.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(got), Some(plane)) => got.eq_ignore_ascii_case(plane),
        _ => false,
    }
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

fn host_is_this_machine(host: &WorkerHost, local: &str) -> bool {
    if !local.is_empty()
        && host
            .hostname
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(local))
    {
        return true;
    }
    let mut advertised = host
        .hardware
        .as_ref()
        .and_then(|h| h.get("addrs"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()));
    let local_ips = local_nonloopback_ips();
    advertised.any(|a| local_ips.iter().any(|b| a.eq_ignore_ascii_case(b)))
}

fn local_nonloopback_ips() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        for tok in String::from_utf8_lossy(&output.stdout).split_whitespace() {
            if tok != "127.0.0.1" && tok != "::1" {
                out.push(tok.to_string());
            }
        }
    }
    out
}

/// True when Origin/Referer host is loopback (`localhost`, 127/8, `::1`).
/// Host only — substring or query `localhost` is not the plane box.
fn origin_is_loopback(headers: &HeaderMap) -> bool {
    for key in [header::ORIGIN, header::REFERER] {
        let Some(raw) = headers.get(key).and_then(|h| h.to_str().ok()) else {
            continue;
        };
        if agent_fleet_contracts::url_is_loopback(raw) {
            return true;
        }
    }
    false
}

fn host_json(state: &AppState, host: &WorkerHost) -> WorkerHostPublic {
    host_json_for_viewer(state, host, false)
}

fn host_json_for_viewer(
    state: &AppState,
    host: &WorkerHost,
    viewer_loopback: bool,
) -> WorkerHostPublic {
    let mut public = host
        .as_public(Utc::now())
        .with_desired(desired_for(state, host));
    // This-machine skip only withholds the staged tarball when the box is
    // already on that package version (`desired_for`). Do not force
    // "current" here — the card compares package versions.
    let plane_box = host_is_this_machine(host, &local_hostname());
    public.is_control_plane_box = plane_box;
    // Only the browser on the plane box (localhost UI) is "this machine".
    // Opening the LAN URL from another PC must not tag the plane.
    public.is_this_machine = plane_box && viewer_loopback;
    public
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

/// LAN URL workers should call. OAuth stays on `API_PUBLIC_BASE` (localhost).
fn worker_api_base(state: &AppState) -> String {
    worker_public_base(&api_base(state))
}

/// `WORKER_PUBLIC_BASE` wins; empty/unset falls back to `api` (`API_PUBLIC_BASE`).
fn worker_public_base(api: &str) -> String {
    std::env::var("WORKER_PUBLIC_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| api.trim_end_matches('/').to_string())
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
    let pair_expires_at = Utc::now() + host_repo::PAIR_TTL;
    (pair_token, pair_token_hash, pair_expires_at)
}

fn created_payload(
    state: &AppState,
    host: &WorkerHost,
    pair_token: &str,
    pair_expires_at: chrono::DateTime<Utc>,
) -> Value {
    let api = worker_api_base(state);
    json!({
        "host": host_json(state, host),
        "pair_token": pair_token,
        "pair_expires_at": pair_expires_at,
        "install_hint": install_hint(&api, pair_token),
    })
}

/// Display name on a host or plane card. DNS hostnames are at most 255 bytes.
const HOST_NAME_MAX: usize = 255;

fn validate_host_name(raw: &str) -> AppResult<String> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name is required"));
    }
    if name.len() > HOST_NAME_MAX {
        return Err(AppError::bad_request("name is too long"));
    }
    Ok(name)
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

/// `cluster` is reserved for `http_bootstrap`. Labels are informational
/// (gpu, linux, …) and must not promote a paired box onto every workspace.
fn label_is_cluster(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("cluster")
}

fn validate_patch_labels(labels: &[String]) -> AppResult<()> {
    if labels.iter().any(|l| label_is_cluster(l)) {
        return Err(AppError::bad_request(
            "cluster is bootstrap-only; labels are informational",
        ));
    }
    Ok(())
}

/// Cloud / public-cloud / Partners are placeholders. HTTP must not create them.
/// Omitted `kind` is the self-hosted native create path.
fn validate_create_kind(raw: Option<&str>) -> AppResult<()> {
    let Some(slug) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    match slug.to_ascii_lowercase().as_str() {
        "host" => Ok(()),
        "cloud" | "aws" | "gcp" | "azure" | "partners" => Err(AppError::bad_request(format!(
            "cannot create a {slug} host; cloud and partners are placeholders"
        ))),
        other => Err(AppError::bad_request(format!(
            "kind must be host; {other} cannot be created"
        ))),
    }
}

const RUNTIME_KEYS: &[&str] = &[
    "work_dir",
    "ollama_models_dir",
    "run_host",
    "run_container",
    "container_backend",
    "docker_image",
    "docker_network",
    "k8s_namespace",
    "k3d_cluster",
    "k3d_memory",
    "k3d_agents",
    "allow_privileged",
    "k8s_cpu",
    "k8s_memory",
    "k8s_ephemeral",
    "egress_mode",
];

fn validate_runtime_path(key: &str, raw: &str) -> AppResult<String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    if s.len() > 512
        || s.chars()
            .any(|c| c.is_control() || c == ';' || c == '|' || c == '&')
    {
        return Err(AppError::bad_request(format!(
            "runtime_settings.{key} looks unsafe"
        )));
    }
    Ok(s.to_string())
}

fn is_secret_runtime_key(k: &str) -> bool {
    matches!(
        k,
        "extra_env" | "machine_token" | "pair_token" | "FLEET_WORKER_TOKEN"
    ) || k.starts_with("FLEET_")
}

/// Shared allowlist for desired (`runtime_settings`) and reported runtime.
/// `reject_unsafe` 400s on ;|& / control / >512 path values (PATCH). Heartbeat
/// omits those keys so the raw object is never persisted.
fn sanitize_runtime_object(v: Value, reject_unsafe: bool) -> AppResult<Value> {
    let Some(obj) = v.as_object() else {
        if reject_unsafe {
            return Err(AppError::bad_request(
                "runtime_settings must be an object".to_string(),
            ));
        }
        return Ok(json!({}));
    };
    let mut out = serde_json::Map::new();
    for (k, val) in obj {
        if is_secret_runtime_key(k) || !RUNTIME_KEYS.contains(&k.as_str()) {
            continue;
        }
        match k.as_str() {
            "run_host" | "run_container" | "allow_privileged" => {
                if let Some(b) = val.as_bool() {
                    out.insert(k.clone(), json!(b));
                }
            }
            "k3d_agents" => {
                if let Some(n) = val.as_i64() {
                    out.insert(k.clone(), json!(n.clamp(0, 16)));
                }
            }
            "container_backend" => {
                let s = val.as_str().unwrap_or("").trim().to_ascii_lowercase();
                if s == "k3d" || s == "docker" {
                    out.insert(k.clone(), json!(s));
                }
            }
            "egress_mode" => {
                let s = val.as_str().unwrap_or("").trim().to_ascii_lowercase();
                if s == "none" || s == "dns_only" || s == "allowlist" {
                    out.insert(k.clone(), json!(s));
                }
            }
            other => {
                let s = val.as_str().unwrap_or("");
                match validate_runtime_path(other, s) {
                    Ok(clean) => {
                        out.insert(other.to_string(), json!(clean));
                    }
                    Err(e) if reject_unsafe => return Err(e),
                    Err(_) => {}
                }
            }
        }
    }
    Ok(Value::Object(out))
}

fn validate_runtime_settings(v: Value) -> AppResult<Value> {
    sanitize_runtime_object(v, true)
}

fn sanitize_runtime_reported(v: Value) -> Value {
    sanitize_runtime_object(v, false).unwrap_or_else(|_| json!({}))
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkerHostBody {
    #[serde(default)]
    pub name: Option<String>,
    /// Product kind. Omitted → self-hosted native. Cloud / aws / gcp / azure /
    /// partners are placeholders and must 400 (not silently become native).
    #[serde(default)]
    pub kind: Option<String>,
    /// Same vocabulary as PATCH. Cloud slugs are not native/sandboxed → 400.
    #[serde(default)]
    pub isolation: Option<String>,
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
    #[serde(default)]
    pub install_model: Option<InstallModelBody>,
    #[serde(default)]
    pub runtime_settings: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct InstallModelBody {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub spec: String,
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
pub struct BootstrapBody {
    pub hostname: String,
    pub platform: String,
    pub advertised_host: bool,
    pub advertised_container: bool,
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Pin a workspace. Omit → oldest workspace (cluster-host).
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
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
    /// Official agent CLIs. Tokens must never appear here.
    #[serde(default)]
    pub agents: Option<serde_json::Value>,
    /// Optional machine snapshot (cores / RAM / OS).
    #[serde(default)]
    pub hardware: Option<serde_json::Value>,
    /// Last `settings_gen` this box applied from a heartbeat response.
    #[serde(default)]
    pub settings_ack_gen: Option<i32>,
    /// Host finished (or failed) the queued model install.
    #[serde(default)]
    pub install_ack: Option<InstallAckBody>,
    /// Current LocalConfig snapshot (work dir, Ollama models dir, k8s knobs).
    #[serde(default)]
    pub runtime: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct InstallAckBody {
    #[serde(default)]
    pub spec: Option<String>,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
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
    /// chat_turn: `{ text, model }` from the official CLI on the runner.
    #[serde(default)]
    pub result: Option<Value>,
}

async fn http_create(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateWorkerHostBody>,
) -> AppResult<Json<Value>> {
    let user = require_owner(&state, &jar, workspace_id).await?;
    validate_create_kind(body.kind.as_deref())?;
    if let Some(raw) = body.isolation.as_deref() {
        let isolation = validate_isolation(raw)?;
        if isolation != fleet_db::models::WORKER_HOST_ISOLATION_NATIVE {
            return Err(AppError::bad_request(
                "create is native-only; isolation must be native",
            ));
        }
    }
    if let Some(n) = body.max_concurrent {
        validate_max_concurrent(n)?;
    }
    // Same bound as PATCH rename. Empty / whitespace / omitted used to
    // insert a pending row with name "" and a flp_ pair token.
    let name = validate_host_name(body.name.as_deref().unwrap_or(""))?;
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

/// Paired at least once: consume-pair or a heartbeat/identity report.
/// Unused pending leftovers have none of these (Cancel / revoke discard).
fn host_ever_paired(host: &WorkerHost) -> bool {
    host.paired_at.is_some()
        || host.last_seen_at.is_some()
        || host
            .hostname
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        || host
            .platform
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
}

/// GET list omits revoke leftovers that never paired. Cancel discards those
/// unused pending rows. A box that paired stays listed as removed.
fn include_in_workspace_host_list(host: &WorkerHost) -> bool {
    host.status != fleet_db::WORKER_HOST_STATUS_DISABLED || host_ever_paired(host)
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let viewer_loopback = origin_is_loopback(&headers);
    let rows = host_repo::list_for_workspace(&state.db, workspace_id).await?;
    let cluster = host_repo::list_cluster(&state.db).await?;
    let plane = plane_public(&state, workspace_id).await?;
    let now = Utc::now();
    let cluster_fresh: Vec<_> = cluster
        .into_iter()
        .filter(|h| {
            h.workspace_id == workspace_id
                || h.last_seen_at
                    .is_some_and(|t| now.signed_duration_since(t) < chrono::Duration::minutes(30))
        })
        .collect();
    let local = local_hostname();
    let this_machine_present = rows
        .iter()
        .chain(cluster_fresh.iter())
        .any(|h| host_is_this_machine(h, &local));
    let mut hosts = Vec::with_capacity(rows.len() + cluster_fresh.len() + 1);
    // Same box as the API: that fleet-host *is* the control plane runner.
    // The leftover synthetic card is hidden.
    if !this_machine_present {
        let mut plane = plane;
        plane.is_this_machine = viewer_loopback;
        hosts.push(plane);
    }
    let mut seen = std::collections::HashSet::new();
    for h in rows.iter().chain(cluster_fresh.iter()) {
        if !include_in_workspace_host_list(h) {
            continue;
        }
        if seen.insert(h.id) {
            hosts.push(host_json_for_viewer(&state, h, viewer_loopback));
        }
    }
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
        "package_versions": {
            "fleet-host": crate::package_versions::fleet_host_version(),
            "fleet-driver": crate::package_versions::fleet_driver_version(),
        },
        "sandbox_backend": sandbox_backend,
    })))
}

async fn load_in_workspace(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
) -> AppResult<WorkerHost> {
    load_visible_host(state, workspace_id, host_id).await
}

/// Workspace-owned, or a cluster host listed on every workspace.
/// GET / chat visibility only — PATCH / DELETE / pair-token use `load_mutable_host`.
pub(crate) async fn load_visible_host(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
) -> AppResult<WorkerHost> {
    if let Some(host) = host_repo::get_in_workspace(&state.db, workspace_id, host_id).await? {
        return Ok(host);
    }
    match host_repo::get(&state.db, host_id).await? {
        Some(host) if host.is_cluster_host() => Ok(host),
        // Same 404 as a missing id — do not name the other workspace.
        _ => Err(AppError::not_found("worker host not found")),
    }
}

/// PATCH / DELETE / pair-token: home workspace only. Cluster guests stay
/// visible on every workspace but must not be paused or revoked there.
async fn load_mutable_host(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
) -> AppResult<WorkerHost> {
    let host = load_visible_host(state, workspace_id, host_id).await?;
    if host.workspace_id != workspace_id {
        return Err(AppError::forbidden(
            "host can only be mutated from its home workspace",
        ));
    }
    Ok(host)
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
    let name = match body.name {
        Some(raw) => Some(validate_host_name(&raw)?),
        None => None,
    };
    if is_control_plane_runner_id(workspace_id, host_id) {
        if let Some(n) = body.max_concurrent {
            validate_max_concurrent(n)?;
        }
        // pause/drain/enabled/auto_update/request_update/isolation are
        // paired-host settings. Plane uses allow_host/allow_container only.
        let row = plane_runners::upsert(
            &state.db,
            workspace_id,
            &plane_runners::PlaneRunnerPatch {
                name,
                allow_host: body.allow_host,
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
    let _ = load_mutable_host(&state, workspace_id, host_id).await?;
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
    if let Some(ref labels) = body.labels {
        validate_patch_labels(labels)?;
    }
    let patch = HostCpSettings {
        name,
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
        runtime_settings: match body.runtime_settings {
            Some(v) => Some(validate_runtime_settings(v)?),
            None => None,
        },
        install_model: match body.install_model {
            Some(m) => {
                let spec = m.spec.trim().to_string();
                let provider = m.provider.trim().to_ascii_lowercase();
                if spec.is_empty() {
                    return Err(AppError::bad_request(
                        "install_model.spec is required (model name or URL)".to_string(),
                    ));
                }
                if provider != "ollama" {
                    return Err(AppError::bad_request(
                        "only ollama model installs are supported from the UI".to_string(),
                    ));
                }
                if spec.len() > 512
                    || spec
                        .chars()
                        .any(|c| c.is_control() || c == ';' || c == '|' || c == '&')
                {
                    return Err(AppError::bad_request(
                        "install_model.spec looks unsafe".to_string(),
                    ));
                }
                Some(json!({ "provider": "ollama", "spec": spec }))
            }
            None => None,
        },
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
    let _ = load_mutable_host(&state, workspace_id, host_id).await?;
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
    let _ = load_mutable_host(&state, workspace_id, host_id).await?;
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
    Ok(Json(paired_payload(&state, &host, &machine_token)))
}

fn bootstrap_secret_from_env() -> Option<String> {
    std::env::var(BOOTSTRAP_SECRET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn require_bootstrap_secret(headers: &HeaderMap) -> AppResult<()> {
    let expected = bootstrap_secret_from_env().ok_or_else(|| AppError::not_found("not found"))?;
    let got = bearer_token(headers)?;
    let exp_hash = host_repo::hash_token(&expected);
    if !host_repo::verify_token(got, &exp_hash) {
        return Err(AppError::unauthorized("invalid bootstrap secret"));
    }
    Ok(())
}

fn paired_payload(state: &AppState, host: &WorkerHost, machine_token: &str) -> Value {
    json!({
        "host_id": host.id,
        "machine_token": machine_token,
        "workspace_id": host.workspace_id,
        "control_plane_url": worker_api_base(state),
        "cp_settings": cp_settings_json(host),
    })
}

async fn http_bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BootstrapBody>,
) -> AppResult<Json<Value>> {
    require_bootstrap_secret(&headers)?;
    let hostname = body.hostname.trim();
    let platform = body.platform.trim();
    if hostname.is_empty() {
        return Err(AppError::bad_request("hostname is required"));
    }
    if platform.is_empty() {
        return Err(AppError::bad_request("platform is required"));
    }
    let workspace_id = match body.workspace_id {
        Some(id) => {
            workspace_repo::min_isolation(&state.db, id).await?;
            id
        }
        None => workspace_repo::first_id(&state.db)
            .await?
            .ok_or_else(|| AppError::conflict("no_workspace"))?,
    };
    let machine_id = parse_machine_id(body.machine_id.as_deref());
    if let Some(mid) = machine_id
        && let Some(existing) = host_repo::find_by_machine_id(&state.db, workspace_id, mid).await?
    {
        let PairedWorkerHost {
            host,
            machine_token,
        } = host_repo::rotate_machine_token(&state.db, existing.id).await?;
        return Ok(Json(paired_payload(&state, &host, &machine_token)));
    }

    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, hostname, None);
    new.allow_host = body.advertised_host;
    new.allow_container = body.advertised_container;
    new.labels = vec!["cluster".into()];
    let (pair_token, pair_token_hash, pair_expires_at) = create_pair_material();
    host_repo::create_pending(&state.db, &new, &pair_token_hash, pair_expires_at).await?;
    let PairedWorkerHost {
        host,
        machine_token,
    } = host_repo::consume_pair_token(
        &state.db,
        &pair_token,
        hostname,
        platform,
        body.advertised_host,
        body.advertised_container,
        machine_id,
    )
    .await
    .map_err(map_pair_err)?;
    Ok(Json(paired_payload(&state, &host, &machine_token)))
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
        body.agents.clone().map(sanitize_heartbeat_agents),
    )
    .await?;
    let host = if let Some(n) = body.settings_ack_gen {
        host_repo::ack_settings(&state.db, host.id, n)
            .await
            .unwrap_or(host)
    } else {
        host
    };
    let host = if let Some(ack) = body.install_ack.as_ref() {
        match ack.spec.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(spec) => host_repo::clear_install_request_if(&state.db, host.id, Some(spec))
                .await
                .unwrap_or(host),
            // Omitted/empty/whitespace spec: keep the queued install.
            None => host,
        }
    } else {
        host
    };
    if let Some(rt) = body.runtime.clone() {
        let _ = host_repo::set_runtime_reported(&state.db, host.id, sanitize_runtime_reported(rt))
            .await;
    }
    let public = host_json(&state, &host);
    let desired = public.desired.clone();
    Ok(Json(json!({
        "host": public,
        "effective": effective_json(&host),
        "desired": desired,
        "install": host.install_request,
    })))
}

fn host_accepts_leases(host: &WorkerHost) -> bool {
    // software_updating must not take new leases; list_assignable still shows the host.
    host.accepts_v1_leases() && !host.software_updating && !host_repo::at_job_cap(host)
}

async fn http_lease(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let host = require_machine(&state, &headers).await?;
    // `claim_next_spawn_for_host` only sees `status=ready`. A host that leased
    // and died (or never completed) must not leave spawn_session / chat_turn
    // leased forever — reclaim before this poll claims.
    work_claims::reclaim_expired_leases(&state.db, RECLAIM_EXPIRED_LIMIT).await?;
    if !host_accepts_leases(&host) {
        return Ok(Json(json!({ "claim": null })));
    }
    // Cluster/bootstrap hosts lease any workspace; paired boxes stay home-only.
    let claim = if host.is_cluster_host() {
        work_claims::claim_next_spawn_for_host(&state.db, host.workspace_id, host.id, LEASE_FOR)
            .await?
    } else {
        work_claims::claim_next_spawn_for_host_in_workspace(
            &state.db,
            host.workspace_id,
            host.id,
            LEASE_FOR,
        )
        .await?
    };
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
    // Held leases still complete when paused/drain/enabled=false. Gating
    // this on host_accepts_leases would 409 a running spawn_session /
    // chat_turn after Stop taking jobs; the claim would sit until expiry.
    let lease_token = Uuid::parse_str(body.lease_token.trim())
        .map_err(|_| AppError::bad_request("lease_token must be a UUID"))?;
    let claim = work_claims::get(&state.db, claim_id)
        .await?
        .ok_or_else(|| AppError::not_found("claim not found"))?;
    // Always consume the lease. Re-queueing the same session_id while the
    // worker_sessions row is still live would double-spawn.
    // Claim workspace, not the host's home workspace — cluster hosts lease
    // jobs for other workspaces.
    let held = if claim.kind == "chat_turn" {
        work_claims::complete_owned_with_result(
            &state.db,
            claim_id,
            lease_token,
            host.id,
            claim.workspace_id,
            body.result.clone(),
        )
        .await?
    } else {
        work_claims::complete_owned(
            &state.db,
            claim_id,
            lease_token,
            host.id,
            claim.workspace_id,
        )
        .await?
    };
    if !held {
        return Err(AppError::conflict("lease not held or already finished"));
    }
    if claim.kind == "chat_turn" {
        return Ok(Json(json!({ "ok": body.ok })));
    }
    let session_id = claim
        .payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    if claim.kind == "signal_session" {
        return Ok(Json(json!({ "ok": true })));
    }
    if claim.kind == "spawn_session" && body.ok {
        // Terminal state comes from the guest /v1 status + inbox. Completing
        // the lease here must not mark done_success (races heartbeat/status).
        return Ok(Json(json!({ "ok": true })));
    }
    if claim.kind == "spawn_session" && !body.ok {
        if let Some(sid) = session_id {
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

#[cfg(test)]
mod host_name_tests {
    use super::{validate_host_name, HOST_NAME_MAX};

    #[test]
    fn empty_and_whitespace_are_required() {
        for raw in ["", "   ", "\t\n", " \u{00a0} "] {
            let err = validate_host_name(raw).expect_err(raw);
            assert!(
                err.to_string().contains("required"),
                "empty/whitespace must be 400 name is required: {raw:?} → {err}"
            );
        }
    }

    #[test]
    fn too_long_is_rejected() {
        let err = validate_host_name(&"x".repeat(HOST_NAME_MAX + 1)).expect_err("256");
        assert!(
            err.to_string().contains("too long"),
            "256-byte name must be 400: {err}"
        );
        let huge = validate_host_name(&"x".repeat(10_000)).expect_err("10k");
        assert!(
            huge.to_string().contains("too long"),
            "10k name must be 400: {huge}"
        );
    }

    #[test]
    fn trims_and_accepts_at_max() {
        assert_eq!(validate_host_name("  studio  ").expect("trim"), "studio");
        let at_max = "x".repeat(HOST_NAME_MAX);
        assert_eq!(
            validate_host_name(&at_max).expect("255"),
            at_max,
            "exactly 255 bytes must be accepted"
        );
    }
}

#[cfg(test)]
mod plane_bake_tests {
    use super::{host_on_manifest_versions, sha_matches_plane, should_withhold_desired};
    use fleet_db::{WorkerBinMeta, WorkerHost, WorkerSoftwareManifest};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn matching_sha_is_plane_bake() {
        assert!(sha_matches_plane(Some("abc123"), Some("abc123")));
        assert!(sha_matches_plane(Some("ABC123"), Some("abc123")));
        assert!(sha_matches_plane(Some("  abc123  "), Some("abc123")));
    }

    #[test]
    fn missing_or_different_sha_is_not_plane_bake() {
        assert!(!sha_matches_plane(None, Some("abc123")));
        assert!(!sha_matches_plane(Some("abc123"), None));
        assert!(!sha_matches_plane(Some(""), Some("abc123")));
        assert!(!sha_matches_plane(Some("abc123"), Some("")));
        assert!(!sha_matches_plane(Some("old-dist"), Some("abc123")));
    }

    fn staged(host_ver: &str, driver_ver: &str) -> WorkerSoftwareManifest {
        let mut bins = BTreeMap::new();
        bins.insert(
            "fleet-host".into(),
            WorkerBinMeta {
                sha256: "staged-host".into(),
                size: 10,
                version: Some(host_ver.into()),
            },
        );
        bins.insert(
            "fleet-driver".into(),
            WorkerBinMeta {
                sha256: "staged-driver".into(),
                size: 12,
                version: Some(driver_ver.into()),
            },
        );
        WorkerSoftwareManifest {
            git_sha: "stage".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins,
        }
    }

    fn plane_host(hostname: &str, bins: Option<serde_json::Value>) -> WorkerHost {
        let now = chrono::Utc::now();
        WorkerHost {
            id: uuid::Uuid::now_v7(),
            workspace_id: uuid::Uuid::now_v7(),
            name: hostname.into(),
            status: "online".into(),
            enabled: true,
            paused: false,
            drain: false,
            isolation: "native".into(),
            allow_host: true,
            allow_container: false,
            max_concurrent: 2,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: None,
            advertised_host: true,
            advertised_container: false,
            platform: Some("linux-x86_64".into()),
            hostname: Some(hostname.into()),
            last_seen_at: Some(now),
            last_error: None,
            current_jobs: 0,
            advertised_max_concurrent: None,
            software_git_sha: None,
            software_bins: bins,
            software_reported_at: Some(now),
            software_updating: false,
            auto_update: true,
            update_requested_at: None,
            machine_id: None,
            hardware: None,
            agent_runtimes: json!([]),
            settings_gen: 0,
            settings_ack_gen: 0,
            install_request: None,
            install_requested_at: None,
            runtime_settings: json!({}),
            runtime_reported: json!({}),
            created_by: None,
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn this_machine_behind_must_receive_desired() {
        let staged = staged("0.0.2", "0.0.2");
        let host = plane_host(
            "viny-5700g",
            Some(json!({
                "fleet-host": {"sha256": null, "version": "0.0.1"},
                "fleet-driver": {"sha256": "abc", "version": "0.0.1"}
            })),
        );
        assert!(
            !should_withhold_desired(&host, &staged, "viny-5700g"),
            "behind plane box must get desired so auto-update can apply"
        );
        assert!(!host_on_manifest_versions(
            host.software_bins.as_ref(),
            &staged
        ));
    }

    #[test]
    fn this_machine_on_current_version_withholds_tarball() {
        let staged = staged("0.0.2", "0.0.2");
        let host = plane_host(
            "viny-5700g",
            Some(json!({
                "fleet-host": {"sha256": "cargo", "version": "0.0.2"},
                "fleet-driver": {"sha256": "cargo-d", "version": "0.0.2"}
            })),
        );
        assert!(should_withhold_desired(&host, &staged, "viny-5700g"));
        let remote = plane_host(
            "x1",
            Some(json!({
                "fleet-host": {"sha256": "cargo", "version": "0.0.2"},
                "fleet-driver": {"sha256": "cargo-d", "version": "0.0.2"}
            })),
        );
        assert!(
            !should_withhold_desired(&remote, &staged, "viny-5700g"),
            "remote hosts still receive desired"
        );
    }

    #[test]
    fn this_machine_without_report_must_receive_desired() {
        let staged = staged("0.0.2", "0.0.2");
        let host = plane_host("viny-5700g", None);
        assert!(!should_withhold_desired(&host, &staged, "viny-5700g"));
    }
}

#[cfg(test)]
mod worker_public_base_tests {
    use super::worker_public_base;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_public_bases<T>(
        worker: Option<&str>,
        api_env: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prev_worker = std::env::var("WORKER_PUBLIC_BASE").ok();
        let prev_api = std::env::var("API_PUBLIC_BASE").ok();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            match worker {
                Some(v) => std::env::set_var("WORKER_PUBLIC_BASE", v),
                None => std::env::remove_var("WORKER_PUBLIC_BASE"),
            }
            match api_env {
                Some(v) => std::env::set_var("API_PUBLIC_BASE", v),
                None => std::env::remove_var("API_PUBLIC_BASE"),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev_worker {
                Some(v) => std::env::set_var("WORKER_PUBLIC_BASE", v),
                None => std::env::remove_var("WORKER_PUBLIC_BASE"),
            }
            match prev_api {
                Some(v) => std::env::set_var("API_PUBLIC_BASE", v),
                None => std::env::remove_var("API_PUBLIC_BASE"),
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Env preference only. HTTP create/pair (`created_payload` /
    /// `worker_api_base`) is locked in worker_hosts_http — do not treat a
    /// hand-built `install_hint(worker)` as that lock.
    #[test]
    fn install_scripts_use_worker_public_base_not_api_fallback() {
        let worker = "http://pair.example.test:9443";
        let api = "http://oauth.example.test:8080";
        assert_ne!(worker, api);

        with_public_bases(Some(worker), Some(api), || {
            assert_eq!(
                worker_public_base(api),
                worker,
                "worker_api_base must prefer WORKER_PUBLIC_BASE over API_PUBLIC_BASE"
            );
            assert_ne!(
                worker_public_base(api),
                api,
                "WORKER_PUBLIC_BASE must win over the API_PUBLIC_BASE fallback"
            );
        });

        with_public_bases(None, Some(api), || {
            assert_eq!(
                worker_public_base(api),
                api,
                "unset WORKER_PUBLIC_BASE falls back to API_PUBLIC_BASE"
            );
        });
        with_public_bases(Some("   "), Some(api), || {
            assert_eq!(
                worker_public_base(api),
                api,
                "blank WORKER_PUBLIC_BASE falls back to API_PUBLIC_BASE"
            );
        });
    }
}

#[cfg(test)]
mod pair_token_once_tests {
    use super::create_pair_material;
    use chrono::{Duration, Utc};
    use fleet_db::repos::worker_hosts as host_repo;

    /// Catalog TTL is one hour. HTTP create/rotate use `create_pair_material`.
    #[test]
    fn pair_token_ttl_is_one_hour() {
        assert_eq!(host_repo::PAIR_TTL, Duration::hours(1));
        assert_eq!(host_repo::PAIR_TTL.num_seconds(), 3600);
        let before = Utc::now();
        let (_token, _hash, expires) = create_pair_material();
        let after = Utc::now();
        assert!(
            expires >= before + host_repo::PAIR_TTL && expires <= after + host_repo::PAIR_TTL,
            "create_pair_material must issue now + 1h, got {expires} (before={before} after={after})"
        );
    }

    /// Hash material only. HTTP create/GET/list contract lives in
    /// `tests/worker_hosts_http.rs` (`pair_token_plaintext_*`).
    #[test]
    fn pair_token_material_hash_is_not_plaintext() {
        let (pair_token, pair_token_hash, _pair_expires_at) = create_pair_material();
        assert!(
            pair_token.starts_with(host_repo::PAIR_TOKEN_PREFIX),
            "pair_token prefix: {pair_token}"
        );
        assert_ne!(
            pair_token, pair_token_hash,
            "at rest must be the hash, not plaintext"
        );
        assert_eq!(pair_token_hash, host_repo::hash_token(&pair_token));
        assert!(
            !pair_token_hash.contains(&pair_token),
            "hash must not embed plaintext"
        );
        assert!(
            !pair_token_hash.contains(host_repo::PAIR_TOKEN_PREFIX),
            "hash must not embed {}: {pair_token_hash}",
            host_repo::PAIR_TOKEN_PREFIX
        );
    }
}

#[cfg(test)]
mod cancel_unused_pending_tests {
    use super::{host_accepts_leases, host_ever_paired, include_in_workspace_host_list};
    use fleet_db::{WorkerHost, WORKER_HOST_STATUS_DISABLED, WORKER_HOST_STATUS_ONLINE};
    use serde_json::json;

    pub fn unused_pending() -> WorkerHost {
        let now = chrono::Utc::now();
        WorkerHost {
            id: uuid::Uuid::now_v7(),
            workspace_id: uuid::Uuid::now_v7(),
            name: "cancel-me".into(),
            status: "pending".into(),
            enabled: true,
            paused: false,
            drain: false,
            isolation: "native".into(),
            allow_host: true,
            allow_container: false,
            max_concurrent: 2,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: None,
            advertised_host: true,
            advertised_container: false,
            platform: None,
            hostname: None,
            last_seen_at: None,
            last_error: None,
            current_jobs: 0,
            advertised_max_concurrent: None,
            software_git_sha: None,
            software_bins: None,
            software_reported_at: None,
            software_updating: false,
            auto_update: true,
            update_requested_at: None,
            machine_id: None,
            hardware: None,
            agent_runtimes: json!([]),
            settings_gen: 0,
            settings_ack_gen: 0,
            install_request: None,
            install_requested_at: None,
            runtime_settings: json!({}),
            runtime_reported: json!({}),
            created_by: None,
            created_at: now,
            paired_at: None,
            updated_at: now,
        }
    }

    /// Columns `list_assignable` SQL requires (enabled, not paused/drain,
    /// native, online, machine token, last_seen). This is the gate *shape*
    /// for the no-Postgres helper — the product lock is HTTP
    /// `cancel_unused_pending_http_discards_never_paired_hidden_not_assignable`
    /// which calls `fleet_db::repos::worker_hosts::list_assignable`.
    fn list_assignable_sql_gates_open(host: &WorkerHost) -> bool {
        host.enabled
            && !host.paused
            && !host.drain
            && host.is_native_isolation()
            && host.status == WORKER_HOST_STATUS_ONLINE
            && host.machine_token_hash.is_some()
            && host.last_seen_at.is_some()
    }

    fn production_src() -> &'static str {
        let src = include_str!("worker_hosts.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    /// Catalog: Cancel discards the unused worker. Revoke never-paired stays
    /// hidden. Must not appear in list_assignable.
    #[test]
    fn cancel_unused_pending_revokes_hidden_not_in_list_assignable() {
        let src = production_src();
        let delete = fn_src(src, "async fn http_delete");
        assert!(
            delete.contains("require_owner"),
            "Cancel/DELETE is owner-only; other-ws must be 403/404:\n{delete}"
        );
        assert!(
            delete.contains("host_repo::revoke"),
            "Cancel is HTTP DELETE → revoke; unused pending must be discarded:\n{delete}"
        );
        assert!(
            !delete.contains("host_repo::delete") && !delete.contains("DELETE FROM worker_hosts"),
            "Cancel is revoke (row stays), not a hard delete:\n{delete}"
        );
        let list = fn_src(src, "async fn http_list");
        assert!(
            list.contains("if !include_in_workspace_host_list(h)"),
            "GET list must drop revoke leftovers via include_in_workspace_host_list:\n{list}"
        );

        let pending = unused_pending();
        assert!(
            include_in_workspace_host_list(&pending),
            "unused pending stays on the list until Cancel (Done keeps it)"
        );
        assert!(
            !host_ever_paired(&pending),
            "create-only pending has never paired"
        );
        assert!(
            !list_assignable_sql_gates_open(&pending),
            "pending must not satisfy list_assignable SQL gates (no token / last_seen / online)"
        );
        assert!(!host_accepts_leases(&pending));

        let mut leftover = pending.clone();
        leftover.status = WORKER_HOST_STATUS_DISABLED.into();
        leftover.machine_token_hash = None;
        leftover.paired_at = None;
        leftover.last_seen_at = None;
        leftover.hostname = None;
        leftover.platform = None;
        leftover.machine_id = None;
        assert!(
            !include_in_workspace_host_list(&leftover),
            "revoke never-paired leftover must stay hidden from GET list"
        );
        leftover.hostname = Some("   ".into());
        leftover.platform = Some("".into());
        assert!(
            !host_ever_paired(&leftover),
            "blank hostname/platform is still never-paired"
        );
        assert!(
            !include_in_workspace_host_list(&leftover),
            "blank identity must not un-hide a Cancel leftover"
        );
        leftover.hostname = None;
        leftover.platform = None;
        assert!(
            !host_ever_paired(&leftover),
            "Cancel leftover is still never-paired"
        );
        assert_ne!(
            leftover.status, WORKER_HOST_STATUS_ONLINE,
            "revoke leftover is not online"
        );
        assert!(
            leftover.machine_token_hash.is_none(),
            "revoke clears the machine token"
        );
        assert!(
            leftover.last_seen_at.is_none(),
            "never-paired leftover has no last_seen"
        );
        assert!(
            !list_assignable_sql_gates_open(&leftover),
            "Cancel leftover must not satisfy list_assignable SQL gates"
        );
        assert!(
            !host_accepts_leases(&leftover),
            "disabled leftover must not take leases"
        );

        let mut paired_removed = leftover.clone();
        paired_removed.paired_at = Some(chrono::Utc::now());
        assert!(
            include_in_workspace_host_list(&paired_removed),
            "revoke of a paired box stays listed (Show removed); only never-paired hides"
        );
        assert!(
            !list_assignable_sql_gates_open(&paired_removed),
            "removed paired box is still not assignable"
        );

        let mut online = pending;
        online.status = WORKER_HOST_STATUS_ONLINE.into();
        online.machine_token_hash = Some("hashed".into());
        online.last_seen_at = Some(chrono::Utc::now());
        online.hostname = Some("box.local".into());
        online.platform = Some("linux-x86_64".into());
        online.paired_at = Some(chrono::Utc::now());
        assert!(include_in_workspace_host_list(&online));
        assert!(
            list_assignable_sql_gates_open(&online),
            "control: a live paired host does satisfy list_assignable SQL gates"
        );
        assert!(host_accepts_leases(&online));

        let mut container_only = online.clone();
        container_only.advertised_host = false;
        container_only.advertised_container = true;
        container_only.allow_container = true;
        assert!(
            !host_accepts_leases(&container_only),
            "paired container-only must not take host-runtime /me/lease"
        );
        let mut cluster_container = container_only.clone();
        cluster_container.labels = vec!["cluster".into()];
        assert!(
            host_accepts_leases(&cluster_container),
            "container-only cluster hosts lease so they can create FleetSandbox CRs"
        );
        let mut both_off = online;
        both_off.advertised_host = false;
        both_off.advertised_container = false;
        assert!(
            !host_accepts_leases(&both_off),
            "both advertised flags false must not take leases"
        );
    }
}

#[cfg(test)]
mod done_keeps_pending_tests {
    use super::cancel_unused_pending_tests::unused_pending;
    use super::include_in_workspace_host_list;

    /// Catalog: Done keeps the unused pending worker (no DELETE). Cancel
    /// discard is the sibling `cancel_unused_*` lock — do not re-clone it.
    #[test]
    fn done_keeps_unused_pending_in_list_without_revoke() {
        let src = include_str!("worker_hosts.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("worker_hosts.rs production source");
        let create = fn_src(prod, "async fn http_create");
        assert!(
            create.contains("host_repo::create_pending") && !create.contains("host_repo::revoke"),
            "Add worker / Done must not revoke on create:\n{create}"
        );
        assert!(
            !prod.contains("async fn http_done") && !prod.contains("fn http_dismiss"),
            "there is no Done HTTP that DELETE/revokes the unused pending host"
        );
        let list = fn_src(prod, "async fn http_list");
        assert!(
            list.contains("if !include_in_workspace_host_list(h)"),
            "GET list (reload) must keep unused pending via include_in_workspace_host_list:\n{list}"
        );
        let delete = fn_src(prod, "async fn http_delete");
        assert!(
            delete.contains("host_repo::revoke"),
            "DELETE is Cancel / Remove, not Done:\n{delete}"
        );

        // UI Done is on_dismiss only. just test excludes fleet-ui, so lock the
        // live PairPanel here — swapping Done onto revoke_host cannot go green.
        let ui = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let ui_prod = ui
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let pair_at = ui_prod.find("fn PairPanel").expect("PairPanel");
        let pair_after = &ui_prod[pair_at..];
        let pair_end = pair_after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(pair_after.len());
        let pair = &pair_after[..pair_end];
        let done_at = pair.find("\"Done\"").expect("Done label");
        let done_start = pair[..done_at].rfind("button {").expect("Done button");
        let done_rest = &pair[done_at + 6..];
        let done_end = done_rest.find("button {").unwrap_or(done_rest.len());
        let done = &pair[done_start..done_at + 6 + done_end];
        assert!(
            done.contains("on_dismiss.call(e)"),
            "Done must dismiss the pair panel, not DELETE:\n{done}"
        );
        assert!(
            !done.contains("revoke_host") && !done.contains("on_revoked"),
            "Done must not revoke the unused pending host:\n{done}"
        );
        assert!(
            ui_prod.contains("on_dismiss: move |_| pair_result.set(None),"),
            "Done clears the overlay only; the pending card stays in hosts"
        );
        assert!(
            pair.contains("revoke_host(&ws, &id)"),
            "Cancel (not Done) is the revoke/DELETE path"
        );

        let pending = unused_pending();
        assert!(
            include_in_workspace_host_list(&pending),
            "Done keeps unused pending on GET list / after reload"
        );
        assert_eq!(pending.status, "pending");
        assert!(pending.paired_at.is_none());
        assert!(pending.machine_token_hash.is_none());
        assert!(pending.last_seen_at.is_none());
        assert!(
            pending
                .hostname
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
                && pending
                    .platform
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty()),
            "Done host is still never-paired (unused)"
        );
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }
}
