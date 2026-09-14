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
use fleet_db::repos::work_claims;
use fleet_db::repos::worker_hosts::{self as host_repo, PairedWorkerHost};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{
    HostCpSettings, NewWorkerHost, WORKER_SOFTWARE_STATUS_CURRENT, WorkerHost, WorkerHostPublic,
    parse_worker_host_isolation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

fn sanitize_model_details(v: Option<&Value>) -> Value {
    let Some(arr) = v.and_then(|x| x.as_array()) else {
        return json!([]);
    };
    json!(
        arr.iter()
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
            .collect::<Vec<_>>()
    )
}

fn sanitize_heartbeat_agents(v: Value) -> Value {
    let Some(arr) = v.as_array() else {
        return json!([]);
    };
    json!(
        arr.iter()
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
            .collect::<Vec<_>>()
    )
}

const PAIR_TTL: Duration = Duration::hours(1);
const LEASE_FOR: Duration = Duration::minutes(2);
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

pub(crate) async fn require_owner(
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

fn send_policy_rank(raw: &str) -> Option<u8> {
    match fleet_db::parse_worker_host_send_policy(raw)? {
        fleet_db::WORKER_HOST_SEND_OWNER_ONLY => Some(0),
        fleet_db::WORKER_HOST_SEND_ALLOWLIST => Some(1),
        fleet_db::WORKER_HOST_SEND_WORKSPACE => Some(2),
        _ => None,
    }
}

fn resolve_send_policy<'a>(wanted: &'a str, ceiling: &str) -> AppResult<&'a str> {
    let wanted = fleet_db::parse_worker_host_send_policy(wanted).ok_or_else(|| {
        AppError::bad_request("send_policy must be owner_only, allowlist, or workspace")
    })?;
    let ceil = fleet_db::parse_worker_host_send_policy(ceiling)
        .unwrap_or(fleet_db::WORKER_HOST_SEND_OWNER_ONLY);
    if send_policy_rank(wanted).unwrap_or(99) > send_policy_rank(ceil).unwrap_or(0) {
        return Err(AppError::bad_request(
            "send_policy cannot be more open than the workspace default",
        ));
    }
    Ok(wanted)
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
    if should_withhold_desired(host, &staged, &local_hostname(), plane_git_sha()) {
        return None;
    }
    Some(staged)
}

/// This-machine skip only when both bins already report the staged versions.
fn should_withhold_desired(
    host: &WorkerHost,
    staged: &fleet_db::WorkerSoftwareManifest,
    local: &str,
    plane: Option<&str>,
) -> bool {
    // Already running this process's bake: staged tarball is often older.
    if host_reports_plane_bake(host, plane) {
        return true;
    }
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

fn host_reports_plane_bake(host: &WorkerHost, plane: Option<&str>) -> bool {
    sha_matches_plane(host.software_git_sha.as_deref(), plane)
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
    local_machine_names().into_iter().next().unwrap_or_default()
}

/// HOSTNAME, `/etc/hostname`, `uname -n`, `hostname` — same needles the
/// tick picker uses so a FQDN vs short name still lists this box.
fn local_machine_names() -> Vec<String> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<String>, raw: &str| {
        let s = raw.trim();
        if s.is_empty() {
            return;
        }
        if !out.iter().any(|n| n.eq_ignore_ascii_case(s)) {
            out.push(s.to_string());
        }
    };
    if let Ok(s) = std::env::var("HOSTNAME") {
        push(&mut out, &s);
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        push(&mut out, &s);
    }
    if let Ok(outp) = std::process::Command::new("uname").arg("-n").output() {
        if outp.status.success() {
            push(&mut out, &String::from_utf8_lossy(&outp.stdout));
        }
    }
    if let Ok(outp) = std::process::Command::new("hostname").output() {
        if outp.status.success() {
            push(&mut out, &String::from_utf8_lossy(&outp.stdout));
        }
    }
    out
}

fn hostnames_match(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let al = a.split('.').next().unwrap_or(a);
    let bl = b.split('.').next().unwrap_or(b);
    !al.is_empty() && al.eq_ignore_ascii_case(bl)
}

fn host_is_this_machine(host: &WorkerHost, local: &str) -> bool {
    let mut names = local_machine_names();
    if !local.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(local)) {
        names.push(local.to_string());
    }
    if let Some(hn) = host
        .hostname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if names.iter().any(|n| hostnames_match(hn, n)) {
            return true;
        }
    }
    host_advertises_local_ip(host)
}

/// Unscoped list/assign/lease: advertised LAN IP ∩ this plane.
/// Hostname alone is impersonable.
fn host_advertises_local_ip(host: &WorkerHost) -> bool {
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

pub(crate) fn origin_is_loopback(headers: &HeaderMap) -> bool {
    for key in [header::ORIGIN, header::REFERER] {
        let Some(raw) = headers.get(key).and_then(|h| h.to_str().ok()) else {
            continue;
        };
        if host_is_loopback_origin(raw) {
            return true;
        }
    }
    false
}

/// Host of Origin/Referer only — not a substring in the query or a
/// `localhost.evil` label.
fn host_is_loopback_origin(raw: &str) -> bool {
    // http::Uri::host() keeps brackets on IPv6 literals (`[::1]`).
    if let Ok(uri) = raw.parse::<axum::http::Uri>() {
        if let Some(host) = uri.host() {
            let host = host.trim_matches(['[', ']']);
            // Host only — a substring match would treat localhost.evil / ?next=localhost as loopback.
            if host.eq_ignore_ascii_case("localhost")
                || host.eq_ignore_ascii_case("127.0.0.1")
                || host.eq_ignore_ascii_case("::1")
                || host.to_ascii_lowercase().starts_with("127.")
            {
                return true;
            }
        }
    }
    let rest = raw.split_once("://").map(|(_, r)| r).unwrap_or(raw);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("").trim();
    let host = if let Some(inner) = hostport.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    let host = host.trim().to_ascii_lowercase();
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("127.")
}

async fn clear_update_request_if_current(state: &AppState, host: WorkerHost) -> WorkerHost {
    if host.update_requested_at.is_none() {
        return host;
    }
    let public = host
        .as_public(Utc::now())
        .with_desired(desired_for(state, &host));
    if public.software_status != WORKER_SOFTWARE_STATUS_CURRENT {
        return host;
    }
    host_repo::clear_update_requested(&state.db, host.id)
        .await
        .unwrap_or(host)
}

fn host_json(state: &AppState, host: &WorkerHost) -> WorkerHostPublic {
    host_json_for_viewer(state, host, false)
}

pub(crate) fn host_json_for_viewer(
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
    if let Some(rt) = public.runtime_reported.as_ref() {
        let clean = sanitize_runtime_object(rt);
        public.runtime_reported =
            Some(clean).filter(|v| v.as_object().is_some_and(|o| !o.is_empty()));
    }
    public
}

fn plane_git_sha() -> Option<&'static str> {
    option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .filter(|s| !s.is_empty())
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
    std::env::var("WORKER_PUBLIC_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| api_base(state))
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

pub(crate) fn create_pair_material() -> (String, String, chrono::DateTime<Utc>) {
    let pair_token = host_repo::generate_pair_token();
    let pair_token_hash = host_repo::hash_token(&pair_token);
    let pair_expires_at = Utc::now() + PAIR_TTL;
    (pair_token, pair_token_hash, pair_expires_at)
}

pub(crate) fn created_payload(
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

/// Keep only allowlisted runtime keys. Drop secrets, unknown keys, and
/// path values that contain shell metacharacters. Used for both desired
/// PATCH (after a 400 on unsafe paths) and reported heartbeat (never 400).
fn sanitize_runtime_object(v: &Value) -> Value {
    let Some(obj) = v.as_object() else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (k, val) in obj {
        if !RUNTIME_KEYS.contains(&k.as_str()) {
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
                if let Some(s) = val.as_str() {
                    if let Ok(clean) = validate_runtime_path(other, s) {
                        out.insert(other.to_string(), json!(clean));
                    }
                }
            }
        }
    }
    Value::Object(out)
}

fn validate_runtime_settings(v: Value) -> AppResult<Value> {
    let Some(obj) = v.as_object() else {
        return Err(AppError::bad_request(
            "runtime_settings must be an object".to_string(),
        ));
    };
    for (k, val) in obj {
        if !RUNTIME_KEYS.contains(&k.as_str()) {
            continue;
        }
        match k.as_str() {
            "run_host" | "run_container" | "allow_privileged" | "k3d_agents"
            | "container_backend" | "egress_mode" => {}
            other => {
                if let Some(s) = val.as_str() {
                    let _ = validate_runtime_path(other, s)?;
                }
            }
        }
    }
    Ok(sanitize_runtime_object(&v))
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
    /// Rejected on create. Cloud / Partners are not worker_hosts rows.
    #[serde(default)]
    pub kind: Option<String>,
    /// Rejected on create. Isolation is a PATCH of a paired native box.
    #[serde(default)]
    pub isolation: Option<String>,
    /// `owner_only` | `allowlist` | `workspace`. Capped by workspace default.
    #[serde(default)]
    pub send_policy: Option<String>,
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
    #[serde(default)]
    pub send_policy: Option<String>,
    #[serde(default)]
    pub send_allowlist: Option<Vec<Uuid>>,
    #[serde(default)]
    pub run_mode: Option<String>,
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
    if body
        .kind
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
    {
        return Err(AppError::bad_request(
            "kind is not a create field (self-hosted pair only)",
        ));
    }
    if body
        .isolation
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
    {
        return Err(AppError::bad_request("isolation must be native"));
    }
    if let Some(n) = body.max_concurrent {
        validate_max_concurrent(n)?;
    }
    // Name is optional. Empty pending rows take the device hostname on pair.
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if name.len() > 255 {
        return Err(AppError::bad_request("name is too long"));
    }
    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, name, Some(user.id));
    let ceiling = workspace_repo::default_send_policy(&state.db, workspace_id).await?;
    let wanted = body
        .send_policy
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ceiling.as_str());
    new.send_policy = resolve_send_policy(wanted, &ceiling)?.to_string();
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
    headers: HeaderMap,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let viewer_loopback = origin_is_loopback(&headers);
    // Home-workspace rows plus this-machine PCs (the CP laptop). Cluster
    // hosts from other workspaces stay off this list and off list_assignable.
    let names = local_machine_names();
    let rows = host_repo::list_visible_for_workspace(&state.db, workspace_id, &names).await?;
    let hosts: Vec<_> = rows
        .iter()
        .filter(|h| {
            h.workspace_id == workspace_id || h.is_cluster_host() || host_advertises_local_ip(h)
        })
        .map(|h| host_json_for_viewer(&state, h, viewer_loopback))
        .collect();
    let packages = crate::install_routes::list_dist_packages(&state);
    Ok(Json(json!({
        "hosts": hosts,
        "packages": packages,
        "plane_git_sha": plane_git_sha(),
        "package_versions": {
            "fleet-host": crate::package_versions::fleet_host_version(),
            "fleet-driver": crate::package_versions::fleet_driver_version(),
        },
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
        Some(host) if host_advertises_local_ip(&host) => Ok(host),
        // Do not name the other workspace — that is a tenant leak.
        Some(_) | None => Err(AppError::not_found("worker host not found")),
    }
}

/// Mutate only from the home workspace. Cluster hosts are visible elsewhere;
/// a regular host in another workspace looks like it does not exist.
async fn require_mutable_host(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
) -> AppResult<WorkerHost> {
    match host_repo::get(&state.db, host_id).await? {
        None => Err(AppError::not_found("worker host not found")),
        Some(h) if h.workspace_id != workspace_id => {
            if h.is_cluster_host() {
                Err(AppError::forbidden("home workspace"))
            } else {
                Err(AppError::not_found("worker host not found"))
            }
        }
        Some(h) => Ok(h),
    }
}

async fn http_get(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WorkerHostPublic>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
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
    let _ = require_mutable_host(&state, workspace_id, host_id).await?;
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
    let name = match body.name {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(AppError::bad_request("name is required"));
            }
            if trimmed.len() > 255 {
                return Err(AppError::bad_request("name is too long"));
            }
            Some(trimmed.to_string())
        }
    };
    let labels = body.labels.map(|ls| {
        ls.into_iter()
            .filter(|l| !fleet_db::label_is_cluster(l))
            .collect()
    });
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
        labels,
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
    let mut host =
        host_repo::patch_cp_settings_in_workspace(&state.db, workspace_id, host_id, &patch).await?;
    if let Some(raw) = body.run_mode.as_deref() {
        if host.current_jobs > 0 {
            return Err(AppError::conflict(
                "run_mode cannot change while jobs are running",
            ));
        }
        let mode = fleet_db::parse_worker_host_run_mode(raw)
            .ok_or_else(|| AppError::bad_request("run_mode must be on_pc or in_container"))?;
        host_repo::set_run_mode(&state.db, host_id, mode).await?;
    }
    if let Some(raw) = body.send_policy.as_deref() {
        let ceiling = workspace_repo::default_send_policy(&state.db, workspace_id).await?;
        let policy = resolve_send_policy(raw, &ceiling)?;
        host = host_repo::set_send_policy(&state.db, host_id, policy).await?;
    }
    if let Some(ids) = body.send_allowlist.as_ref() {
        if host.send_policy != fleet_db::WORKER_HOST_SEND_ALLOWLIST {
            return Err(AppError::bad_request(
                "send_allowlist requires send_policy=allowlist",
            ));
        }
        for uid in ids {
            if !workspace_repo::is_member(&state.db, workspace_id, *uid).await? {
                return Err(AppError::bad_request(
                    "allowlisted user must be a workspace member",
                ));
            }
        }
        let user = require_user(&auth_state(&state), &jar).await?;
        host_repo::replace_send_allowlist(&state.db, host_id, ids, user.id).await?;
        host = host_repo::get(&state.db, host_id)
            .await?
            .ok_or_else(|| AppError::not_found("worker host not found"))?;
    }
    Ok(Json(host_json(&state, &host)))
}

async fn http_delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WorkerHostPublic>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    let _ = require_mutable_host(&state, workspace_id, host_id).await?;
    let host = host_repo::revoke_in_workspace(&state.db, workspace_id, host_id).await?;
    Ok(Json(host_json(&state, &host)))
}

async fn http_new_pair_token(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, host_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_owner(&state, &jar, workspace_id).await?;
    let _ = require_mutable_host(&state, workspace_id, host_id).await?;
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
    new.run_mode = fleet_db::WORKER_HOST_RUN_IN_CONTAINER.to_string();
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
    let host = if let Some(hn) = hostname {
        host_repo::adopt_hostname_if_placeholder(&state.db, host.id, hn)
            .await
            .unwrap_or(host)
    } else {
        host
    };
    let host = if let Some(n) = body.settings_ack_gen {
        host_repo::ack_settings(&state.db, host.id, n)
            .await
            .unwrap_or(host)
    } else {
        host
    };
    let host = if let Some(ack) = body.install_ack.as_ref() {
        host_repo::clear_install_request_if(
            &state.db,
            host.id,
            ack.spec.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        )
        .await
        .unwrap_or(host)
    } else {
        host
    };
    if let Some(rt) = body.runtime.clone() {
        let _ =
            host_repo::set_runtime_reported(&state.db, host.id, sanitize_runtime_object(&rt)).await;
    }
    let host = clear_update_request_if_current(&state, host).await;
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
    if !host.enabled
        || host.paused
        || host.drain
        || !host.is_native_isolation()
        || host.status != fleet_db::WORKER_HOST_STATUS_ONLINE
        || host.software_updating
        || host_repo::at_job_cap(host)
    {
        return false;
    }
    match host.run_mode.as_str() {
        fleet_db::WORKER_HOST_RUN_IN_CONTAINER => host.effective_container(),
        _ => host.effective_host(),
    }
}

async fn http_lease(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let host = require_machine(&state, &headers).await?;
    if !host_accepts_leases(&host) {
        return Ok(Json(json!({ "claim": null })));
    }
    // Lease poll is the reclaim path. A sweeper-only helper would never run
    // in this process; expired rows must return to ready before this claim.
    let _ = work_claims::reclaim_expired_leases(&state.db, 64).await;
    let claim = if host.is_cluster_host() || host_advertises_local_ip(&host) {
        // Cluster and the CP laptop take jobs from any workspace.
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
            let applied = state
                .orch
                .store
                .complete_spawn_failure(sid)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
            if applied {
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
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod plane_bake_tests {
    use super::{
        host_on_manifest_versions, origin_is_loopback, sha_matches_plane, should_withhold_desired,
    };
    use axum::http::{HeaderMap, HeaderValue, header};
    use fleet_db::{WorkerBinMeta, WorkerHost, WorkerSoftwareManifest};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn headers(key: axum::http::HeaderName, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(key, HeaderValue::from_str(value).expect("header value"));
        h
    }

    #[test]
    fn origin_is_loopback_parses_host_not_substring() {
        for origin in [
            "http://localhost:3030",
            "http://127.0.0.1:3030",
            "http://[::1]:3030",
        ] {
            assert!(
                origin_is_loopback(&headers(header::ORIGIN, origin)),
                "loopback Origin {origin}"
            );
        }
        for origin in [
            "http://localhost.evil.example.test",
            "http://evil-127.0.0.1.example.test",
            "http://192.168.1.50:3030",
        ] {
            assert!(
                !origin_is_loopback(&headers(header::ORIGIN, origin)),
                "non-loopback Origin {origin}"
            );
        }
        assert!(
            !origin_is_loopback(&headers(
                header::REFERER,
                "http://192.168.1.50:3030/?next=http://localhost:3030"
            )),
            "Referer query localhost must not count as loopback"
        );
        assert!(
            origin_is_loopback(&headers(header::REFERER, "http://[::1]:3030")),
            "Referer [::1] is loopback"
        );
        assert!(
            !origin_is_loopback(&HeaderMap::new()),
            "no Origin/Referer is not loopback"
        );
    }

    #[test]
    fn origin_host_not_substring() {
        assert!(super::host_is_loopback_origin("http://localhost:3030"));
        assert!(super::host_is_loopback_origin("http://127.0.0.1:3030"));
        assert!(super::host_is_loopback_origin("http://[::1]:3030"));
        assert!(!super::host_is_loopback_origin(
            "http://localhost.evil.example.test"
        ));
        assert!(!super::host_is_loopback_origin(
            "http://192.168.1.50:3030/?next=http://localhost:3030"
        ));
    }

    #[test]
    fn matching_sha_is_plane_bake() {
        for (got, plane, case) in [
            (Some("abc123"), Some("abc123"), "exact"),
            (Some("ABC123"), Some("abc123"), "case-insensitive"),
            (Some("  abc123  "), Some("abc123"), "trimmed"),
        ] {
            assert!(
                sha_matches_plane(got, plane),
                "{case} reported sha is this process bake: got={got:?} plane={plane:?}"
            );
        }
    }

    #[test]
    fn sha_matches_plane_rejects_dirty_tree_id_suffix() {
        // scripts/tree-id.sh: clean is 40 lowercase hex; dirty is HEAD-<12hex>.
        let clean = "0123456789abcdef0123456789abcdef01234567";
        let dirty = "0123456789abcdef0123456789abcdef01234567-aaaaaaaaaaaa";
        assert!(
            !sha_matches_plane(Some(dirty), Some(clean)),
            "dirty host bake {dirty} is not the clean plane bake {clean}"
        );
    }

    #[test]
    fn should_withhold_desired_when_reported_sha_matches_plane_ignoring_case_and_trim() {
        let staged = staged("0.0.2", "0.0.2");
        let mut host = plane_host(
            "x1",
            Some(json!({
                "fleet-host": {"sha256": null, "version": "0.0.1"},
                "fleet-driver": {"sha256": "abc", "version": "0.0.1"}
            })),
        );
        host.software_git_sha = Some(" ABC123 ".into());
        assert!(
            should_withhold_desired(&host, &staged, "viny-5700g", Some("abc123")),
            "remote host behind package versions still withholds when software_git_sha matches plane after trim+case"
        );
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
            runner_kind: fleet_db::RUNNER_KIND_SELF_HOSTED.into(),
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
            send_policy: fleet_db::WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            run_mode: fleet_db::WORKER_HOST_RUN_ON_PC.to_string(),
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
            !should_withhold_desired(&host, &staged, "viny-5700g", None),
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
        assert!(should_withhold_desired(&host, &staged, "viny-5700g", None));
        let remote = plane_host(
            "x1",
            Some(json!({
                "fleet-host": {"sha256": "cargo", "version": "0.0.2"},
                "fleet-driver": {"sha256": "cargo-d", "version": "0.0.2"}
            })),
        );
        assert!(
            !should_withhold_desired(&remote, &staged, "viny-5700g", None),
            "remote hosts still receive desired"
        );
    }

    #[test]
    fn this_machine_without_report_must_receive_desired() {
        let staged = staged("0.0.2", "0.0.2");
        let host = plane_host("viny-5700g", None);
        assert!(!should_withhold_desired(&host, &staged, "viny-5700g", None));
    }

    #[test]
    fn host_on_this_process_bake_withholds_even_when_remote() {
        let staged = staged("0.0.2", "0.0.2");
        let mut host = plane_host("x1", None);
        host.software_git_sha = Some("abc123".into());
        assert!(
            should_withhold_desired(&host, &staged, "viny-5700g", Some("abc123")),
            "a host already on this process bake must not get an older staged tarball"
        );
    }

    #[test]
    fn remote_other_bake_sha_still_receives_desired() {
        let staged = staged("0.0.2", "0.0.2");
        let mut host = plane_host(
            "x1",
            Some(json!({
                "fleet-host": {"sha256": "cargo", "version": "0.0.2"},
                "fleet-driver": {"sha256": "cargo-d", "version": "0.0.2"}
            })),
        );
        host.software_git_sha = Some("old-dist".into());
        assert!(
            !should_withhold_desired(&host, &staged, "viny-5700g", Some("abc123")),
            "remote on a different bake still receives desired even when package versions match"
        );
    }

    #[test]
    fn this_machine_behind_but_on_plane_bake_still_withholds() {
        let staged = staged("0.0.2", "0.0.2");
        let mut host = plane_host(
            "viny-5700g",
            Some(json!({
                "fleet-host": {"sha256": null, "version": "0.0.1"},
                "fleet-driver": {"sha256": "abc", "version": "0.0.1"}
            })),
        );
        host.software_git_sha = Some("abc123".into());
        assert!(
            should_withhold_desired(&host, &staged, "viny-5700g", Some("abc123")),
            "plane bake withholds even when this-machine package versions are behind"
        );
    }

    #[test]
    fn this_machine_matches_fqdn_vs_short_hostname() {
        // Do not use this box's real hostname — local_machine_names()
        // would make the negative case true.
        let host = plane_host("unique-box-xyz.lan", None);
        assert!(super::host_is_this_machine(&host, "unique-box-xyz"));
        assert!(super::host_is_this_machine(&host, "UNIQUE-BOX-XYZ"));
        let short = plane_host("unique-box-xyz", None);
        assert!(super::host_is_this_machine(&short, "unique-box-xyz.lan"));
        assert!(!super::host_is_this_machine(&short, "other-box"));
        assert!(
            !super::host_advertises_local_ip(&short),
            "hostname-only must not unscoped-share"
        );
    }
}
