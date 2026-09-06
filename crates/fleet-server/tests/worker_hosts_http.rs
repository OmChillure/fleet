//! HTTP tests for paired worker hosts and public install scripts.
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use fleet_auth::{AuthConfig, ACCESS_COOKIE, REFRESH_COOKIE};
use fleet_db::{connect_and_migrate_with, PoolConfig};
use fleet_integrations::{CredentialsKey, IntegrationsConfig};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{
    build_http_app, AppState, ClaudeConfig, GrokConfig, OllamaConfig, OpenaiConfig, ServerConfig,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_PASSWORD: &str = "correct-horse";
const TEST_ADMIN_TOKEN: &str = "test-admin";
const TEST_JWT_SECRET: &str = "dev-change-me-to-a-long-random-secret-at-least-32-chars";
const TEST_BOOTSTRAP_SECRET: &str = "test-bootstrap-secret";

fn require_database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "DATABASE_URL is required for this integration test. \
                 Start Postgres (just db-up) and export DATABASE_URL."
            )
        })
}

fn unique_email() -> String {
    format!("srv.{}@example.test", Uuid::new_v4())
}

fn test_auth_config() -> AuthConfig {
    AuthConfig {
        jwt_secret: TEST_JWT_SECRET.into(),
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(604_800),
        cookie_secure: false,
        cookie_samesite: cookie::SameSite::Lax,
        signup_token: None,
    }
}

fn test_integrations_config(jwt_secret: &str) -> IntegrationsConfig {
    IntegrationsConfig {
        api_public_base: "http://localhost:8080".into(),
        ui_public_base: "http://localhost:3030".into(),
        state_secret: jwt_secret.to_string(),
        credentials_key: CredentialsKey::from_secret(jwt_secret).expect("credentials key"),
        github: None,
        gitlab: None,
        slack: None,
        linear: None,
        jira: None,
        figma: None,
        notion: None,
        google_drive: None,
        teams: None,
        discord: None,
        trello: None,
        asana: None,
    }
}

/// Serializes WORKER_PUBLIC_BASE / API_PUBLIC_BASE mutation so sibling tests
/// keep the unset-WORKER fallback (`control_plane_url` = state's API base).
fn pub_base_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct PublicBaseEnvGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    prev_worker: Option<String>,
    prev_api: Option<String>,
}

impl PublicBaseEnvGuard {
    async fn lock_and_set(worker: Option<&str>, api: Option<&str>) -> Self {
        let lock = pub_base_env_lock().lock().await;
        let prev_worker = std::env::var("WORKER_PUBLIC_BASE").ok();
        let prev_api = std::env::var("API_PUBLIC_BASE").ok();
        set_public_base_env(worker, api);
        Self {
            _lock: lock,
            prev_worker,
            prev_api,
        }
    }

    fn set_worker_public_base(&self, worker: Option<&str>) {
        // SAFETY: this guard holds pub_base_env_lock; Drop restores both vars.
        unsafe {
            match worker {
                Some(v) => std::env::set_var("WORKER_PUBLIC_BASE", v),
                None => std::env::remove_var("WORKER_PUBLIC_BASE"),
            }
        }
    }
}

impl Drop for PublicBaseEnvGuard {
    fn drop(&mut self) {
        set_public_base_env(self.prev_worker.as_deref(), self.prev_api.as_deref());
    }
}

fn set_public_base_env(worker: Option<&str>, api: Option<&str>) {
    // SAFETY: caller holds pub_base_env_lock; Drop restores prior values.
    unsafe {
        match worker {
            Some(v) => std::env::set_var("WORKER_PUBLIC_BASE", v),
            None => std::env::remove_var("WORKER_PUBLIC_BASE"),
        }
        match api {
            Some(v) => std::env::set_var("API_PUBLIC_BASE", v),
            None => std::env::remove_var("API_PUBLIC_BASE"),
        }
    }
}

/// Serializes FLEET_BOOTSTRAP_SECRET mutation so siblings keep
/// TEST_BOOTSTRAP_SECRET (or whatever was already configured).
fn bootstrap_secret_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct BootstrapSecretEnvGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    prev: Option<String>,
}

impl BootstrapSecretEnvGuard {
    async fn lock_and_set(value: Option<&str>) -> Self {
        let lock = bootstrap_secret_env_lock().lock().await;
        let prev = std::env::var("FLEET_BOOTSTRAP_SECRET").ok();
        set_bootstrap_secret_env(value);
        Self { _lock: lock, prev }
    }

    fn set(&self, value: Option<&str>) {
        // SAFETY: this guard holds bootstrap_secret_env_lock; Drop restores.
        set_bootstrap_secret_env(value);
    }

    fn configured_secret(&self) -> String {
        self.prev
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(TEST_BOOTSTRAP_SECRET)
            .to_string()
    }
}

impl Drop for BootstrapSecretEnvGuard {
    fn drop(&mut self) {
        set_bootstrap_secret_env(self.prev.as_deref());
    }
}

fn set_bootstrap_secret_env(value: Option<&str>) {
    // SAFETY: caller holds bootstrap_secret_env_lock; Drop restores prior value.
    unsafe {
        match value {
            Some(v) => std::env::set_var("FLEET_BOOTSTRAP_SECRET", v),
            None => std::env::remove_var("FLEET_BOOTSTRAP_SECRET"),
        }
    }
}

/// In-process app + state (copied from http_api.rs; do not import private items).
async fn app_with_state() -> (axum::Router, AppState) {
    app_with_state_opts(None, None).await
}

async fn app_with_state_dist(worker_dist_dir: Option<PathBuf>) -> (axum::Router, AppState) {
    app_with_state_opts(worker_dist_dir, None).await
}

async fn app_with_state_opts(
    worker_dist_dir: Option<PathBuf>,
    api_public_base: Option<&str>,
) -> (axum::Router, AppState) {
    {
        // Do not call app_with_state while holding BootstrapSecretEnvGuard —
        // this mutex is not reentrant.
        let _secret = bootstrap_secret_env_lock().lock().await;
        if std::env::var("FLEET_BOOTSTRAP_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_none()
        {
            // SAFETY: held under bootstrap_secret_env_lock; siblings restore.
            unsafe {
                std::env::set_var("FLEET_BOOTSTRAP_SECRET", TEST_BOOTSTRAP_SECRET);
            }
        }
    }
    let database_url = require_database_url();

    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);

    let orch_cfg = OrchConfig {
        database_url: database_url.clone(),
        admin_token: TEST_ADMIN_TOKEN.into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
        orchestrator_public_url: "http://127.0.0.1:0".into(),
        data_dir,
        tick_interval: Duration::from_secs(5),
        max_agents: 3,
        heartbeat_stale: Duration::from_secs(60),
        spawn_timeout: Duration::from_secs(30),
        kill_grace: Duration::from_secs(0),
        default_task_timeout: Duration::from_secs(3600),
        default_max_retries: 2,
        retry_backoff: Duration::from_secs(10),
        on_task_fail: "fail_run".into(),
        auto_sync_external: false,
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    };

    let auth = test_auth_config();
    let jwt_secret = auth.jwt_secret.clone();
    let mut integrations = test_integrations_config(&jwt_secret);
    if let Some(base) = api_public_base {
        integrations.api_public_base = base.trim().trim_end_matches('/').to_string();
    }
    let config = ServerConfig {
        database_url: database_url.clone(),
        bind: "127.0.0.1:0".parse().expect("bind"),
        cors_origins: vec![
            "http://localhost:3030".into(),
            "http://127.0.0.1:3030".into(),
        ],
        auth,
        integrations,
        ollama: OllamaConfig {
            base_url: "http://127.0.0.1:11434".into(),
            default_model: None,
            timeout: Duration::from_secs(5),
            enabled: false,
        },
        grok: GrokConfig {
            enabled: false,
            auth_path: PathBuf::from("/tmp/fleet-server-http-api-no-grok-auth.json"),
            api_key: None,
            api_base: "https://api.x.ai/v1".into(),
        },
        openai: OpenaiConfig {
            enabled: true,
            api_key: None,
            api_base: "https://api.openai.com/v1".into(),
        },
        claude: ClaudeConfig {
            enabled: true,
            api_key: None,
            api_base: "https://api.anthropic.com".into(),
        },
        worker_dist_dir,
    };

    let db = connect_and_migrate_with(
        &database_url,
        &PoolConfig {
            max_connections: 5,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(30)),
            max_lifetime: None,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("connect_and_migrate failed: {e}"));

    let orch = build_app_state(&orch_cfg)
        .await
        .unwrap_or_else(|e| panic!("orchestrator state: {e:#}"));

    let state = AppState {
        db,
        auth: config.auth.clone(),
        integrations: config.integrations.clone(),
        orch: orch.clone(),
        ollama: config.ollama.clone(),
        grok: config.grok.clone(),
        openai: config.openai.clone(),
        claude: config.claude.clone(),
        worker_dist_dir: config.worker_dist_dir.clone(),
    };

    let router = build_http_app(state.clone(), orch, &config).expect("build_http_app");
    (router, state)
}

async fn app() -> axum::Router {
    app_with_state().await.0
}

#[derive(Debug, Clone)]
struct ParsedSetCookie {
    name: String,
    value: String,
    raw: String,
}

impl ParsedSetCookie {
    fn is_httponly(&self) -> bool {
        self.raw
            .split(';')
            .any(|part| part.trim().eq_ignore_ascii_case("HttpOnly"))
    }
}

fn parse_set_cookies(headers: &HeaderMap) -> Vec<ParsedSetCookie> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|raw| {
            let nv = raw.split(';').next().unwrap_or(raw);
            let (name, value) = nv.split_once('=')?;
            Some(ParsedSetCookie {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                raw: raw.to_string(),
            })
        })
        .collect()
}

/// Client-side cookie jar for `fleet_access` / `fleet_refresh`.
#[derive(Debug, Clone, Default)]
struct CookieJar {
    values: BTreeMap<String, String>,
}

impl CookieJar {
    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn access(&self) -> Option<&str> {
        self.get(ACCESS_COOKIE)
    }

    fn refresh(&self) -> Option<&str> {
        self.get(REFRESH_COOKIE)
    }

    fn absorb(&mut self, headers: &HeaderMap) {
        for c in parse_set_cookies(headers) {
            if c.value.is_empty() {
                self.values.remove(&c.name);
            } else {
                self.values.insert(c.name, c.value);
            }
        }
    }

    fn cookie_header(&self) -> Option<String> {
        if self.values.is_empty() {
            return None;
        }
        Some(
            self.values
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[derive(Debug)]
struct HttpOut {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    raw: String,
}

impl HttpOut {
    fn cookie(&self, name: &str) -> Option<ParsedSetCookie> {
        parse_set_cookies(&self.headers)
            .into_iter()
            .find(|c| c.name == name)
    }

    fn absorb_into(&self, jar: &mut CookieJar) {
        jar.absorb(&self.headers);
    }
}

async fn call_inner(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    bearer: Option<&str>,
    body: Option<Value>,
) -> HttpOut {
    call_inner_headers(app, method, uri, cookies, bearer, &[], body).await
}

async fn call_inner_headers(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    bearer: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: Option<Value>,
) -> HttpOut {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(jar) = cookies {
        if let Some(h) = jar.cookie_header() {
            builder = builder.header(header::COOKIE, h);
        }
    }
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let req = builder
        .body(match body {
            Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let res = app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "raw": raw }))
    };
    HttpOut {
        status,
        headers,
        body,
        raw,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    body: Option<Value>,
) -> HttpOut {
    call_inner(app, method, uri, cookies, None, body).await
}

async fn call_with_headers(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    extra_headers: &[(&str, &str)],
    body: Option<Value>,
) -> HttpOut {
    call_inner_headers(app, method, uri, cookies, None, extra_headers, body).await
}

async fn call_bearer(
    app: &axum::Router,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<Value>,
) -> HttpOut {
    call_inner(app, method, uri, None, Some(bearer), body).await
}

async fn signup(app: &axum::Router, email: &str, password: &str) -> HttpOut {
    call(
        app,
        "POST",
        "/api/auth/signup",
        None,
        Some(json!({ "email": email, "password": password })),
    )
    .await
}

async fn login(app: &axum::Router, email: &str, password: &str) -> (HttpOut, CookieJar) {
    let out = call(
        app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": email, "password": password })),
    )
    .await;
    let mut jar = CookieJar::default();
    out.absorb_into(&mut jar);
    (out, jar)
}

/// Signup + login. Returns unique email, cookie jar, and default workspace id.
async fn signup_and_login(app: &axum::Router) -> (String, CookieJar, String) {
    let email = unique_email();
    let created = signup(app, &email, TEST_PASSWORD).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "signup: {}",
        created.body
    );
    let (login_out, jar) = login(app, &email, TEST_PASSWORD).await;
    assert_eq!(
        login_out.status,
        StatusCode::OK,
        "login: {}",
        login_out.body
    );
    assert!(
        login_out.cookie(ACCESS_COOKIE).is_some(),
        "login must set {ACCESS_COOKIE}: {:?}",
        parse_set_cookies(&login_out.headers)
    );
    assert!(
        login_out.cookie(REFRESH_COOKIE).is_some(),
        "login must set {REFRESH_COOKIE}: {:?}",
        parse_set_cookies(&login_out.headers)
    );
    let access_c = login_out.cookie(ACCESS_COOKIE).unwrap();
    assert!(
        access_c.is_httponly(),
        "{ACCESS_COOKIE} must be HttpOnly: {}",
        access_c.raw
    );
    assert!(jar.access().is_some(), "jar missing {ACCESS_COOKIE}");
    assert!(jar.refresh().is_some(), "jar missing {REFRESH_COOKIE}");
    let ws_id = login_out.body["default_workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("default_workspace.id: {}", login_out.body))
        .to_string();
    (email, jar, ws_id)
}

fn hosts_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/worker-hosts")
}

#[tokio::test]
async fn unauthenticated_create_is_401() {
    let app = app().await;
    let ws = Uuid::new_v4();
    let out = call(
        &app,
        "POST",
        &hosts_path(&ws.to_string()),
        None,
        Some(json!({ "name": "studio" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
}

#[tokio::test]
async fn owner_create_and_get_list() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "studio" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(created.body["host"]["name"], "studio");
    assert_eq!(created.body["host"]["status"], "pending");
    assert_eq!(
        created.body["host"]["isolation"], "native",
        "new hosts default to native isolation: {}",
        created.body
    );
    assert_eq!(
        created.body["host"]["workspace_id"].as_str(),
        Some(ws.as_str())
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );
    assert!(created.body["pair_expires_at"].is_string());
    let hint = created.body["install_hint"].as_str().unwrap_or("");
    assert!(hint.contains("/install-worker.sh"), "install_hint: {hint}");
    assert!(
        hint.contains("FLEET_PAIR_TOKEN="),
        "install_hint must pair in the same command: {hint}"
    );
    assert!(hint.contains(&pair_token), "install_hint includes token");

    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    let hosts = list.body["hosts"].as_array().expect("hosts array");
    assert!(
        hosts.iter().any(|h| h["id"].as_str() == Some(hid.as_str())),
        "list missing created host: {}",
        list.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(got.body["name"], "studio");
    assert_eq!(got.body["isolation"], "native");
    assert!(got.body.get("machine_token_hash").is_none());
    assert!(got.body.get("pair_token").is_none());
}

/// catalog:list-worker-hosts-list-other-ws-403 — GET list/detail in
/// another workspace is 403 not-a-member and must not leak inventory.
/// Distinct from owner_can_rename_host (PATCH A's hid on B's path 404 /
/// B on A's path 403), member_can_list_but_cannot_mutate (same-ws member),
/// plane_runner_delete_foreign_id_is_404 (plane UUID), and
/// list-chat-foreign-runner-id-404 (chat/status?runner_id).
fn assert_other_ws_host_get_is_403_and_omits_inventory(
    out: &HttpOut,
    when: &str,
    hid: &str,
    name: &str,
    hostname: &str,
    platform: &str,
    hardware_needle: &str,
    software_needle: &str,
    machine_token: &str,
) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{when} must not 200 with A's card: {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{when} must be 403 not-a-member: {}",
        out.raw
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("not a member"),
        "{when} should say not a member, got {err:?}: {}",
        out.raw
    );
    for (label, needle) in [
        ("host id", hid),
        ("name", name),
        ("hostname", hostname),
        ("platform", platform),
        ("hardware", hardware_needle),
        ("software", software_needle),
        ("machine_token", machine_token),
    ] {
        assert!(
            !needle.is_empty()
                && !out.raw.contains(needle)
                && !json_contains_str(&out.body, needle),
            "{when} must not contain A's {label} ({needle}): {}",
            out.raw
        );
    }
    for key in ["pair_token", "machine_token_hash", "machine_token"] {
        assert!(
            !json_has_key(&out.body, key),
            "{when} leaked {key}: {}",
            out.body
        );
    }
    assert!(
        !out.raw.contains("flp_") && !json_contains_str(&out.body, "flp_"),
        "{when} must not contain pair_token (flp_): {}",
        out.raw
    );
    if let Some(hosts) = out.body["hosts"].as_array() {
        assert!(
            hosts.iter().all(|h| h["id"].as_str() != Some(hid)),
            "{when} must not include A's host card: {}",
            out.body
        );
    }
}

#[tokio::test]
async fn worker_hosts_list_other_ws_is_403_and_leaks_no_inventory() {
    let app = app().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    let hid = a_hid.to_string();
    assert!(
        a_machine.starts_with("flm_"),
        "pair must mint a real machine token: {a_machine}"
    );

    let stamp = Uuid::new_v4();
    let hardware_os = format!("a-hw-os-{stamp}");
    let software_sha = format!("a-sw-sha-{stamp}");
    let inventory = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &a_machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "hardware": {
                "cpu_cores": 8,
                "mem_total_bytes": 16_000_000_000u64,
                "os": hardware_os,
            },
            "software": { "git_sha": software_sha, "updating": false },
        })),
    )
    .await;
    assert_eq!(
        inventory.status,
        StatusCode::OK,
        "A inventory heartbeat: {}",
        inventory.body
    );

    let list_a = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_a), None).await;
    assert_eq!(
        list_a.status,
        StatusCode::OK,
        "A GET /worker-hosts snapshot: {}",
        list_a.body
    );
    assert!(
        !json_has_key(&list_a.body, "pair_token"),
        "A GET list must not include pair_token: {}",
        list_a.body
    );
    let a_card = list_a.body["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("A list missing created host {hid}: {}", list_a.body));
    assert_eq!(
        a_card["name"].as_str(),
        Some(a_name.as_str()),
        "A list name: {}",
        list_a.body
    );
    assert!(
        a_card.get("pair_token").is_none(),
        "A list card must not include pair_token: {a_card}"
    );
    let a_hostname = a_card["hostname"]
        .as_str()
        .unwrap_or_else(|| panic!("A list hostname after pair: {a_card}"))
        .to_string();
    let a_platform = a_card["platform"]
        .as_str()
        .unwrap_or_else(|| panic!("A list platform after pair: {a_card}"))
        .to_string();
    assert_eq!(
        a_card["hardware"]["os"].as_str(),
        Some(hardware_os.as_str()),
        "A list hardware: {a_card}"
    );
    assert_eq!(
        a_card["software"]["git_sha"].as_str(),
        Some(software_sha.as_str()),
        "A list software: {a_card}"
    );

    let got_a = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws_a)),
        Some(&jar_a),
        None,
    )
    .await;
    assert_eq!(
        got_a.status,
        StatusCode::OK,
        "A GET /worker-hosts/{{hid}} snapshot: {}",
        got_a.body
    );
    assert_eq!(got_a.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(got_a.body["name"].as_str(), Some(a_name.as_str()));
    assert_eq!(got_a.body["hostname"].as_str(), Some(a_hostname.as_str()));
    assert_eq!(got_a.body["platform"].as_str(), Some(a_platform.as_str()));
    assert_eq!(
        got_a.body["hardware"]["os"].as_str(),
        Some(hardware_os.as_str())
    );
    assert_eq!(
        got_a.body["software"]["git_sha"].as_str(),
        Some(software_sha.as_str())
    );
    assert!(
        got_a.body.get("pair_token").is_none() && !json_has_key(&got_a.body, "pair_token"),
        "A GET detail must not include pair_token: {}",
        got_a.body
    );
    assert!(
        got_a.body.get("machine_token_hash").is_none()
            && !json_has_key(&got_a.body, "machine_token_hash"),
        "A GET detail must not include machine_token_hash: {}",
        got_a.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let b_on_a_list = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_b), None).await;
    assert_other_ws_host_get_is_403_and_omits_inventory(
        &b_on_a_list,
        "B GET /worker-hosts on A's path",
        &hid,
        &a_name,
        &a_hostname,
        &a_platform,
        &hardware_os,
        &software_sha,
        &a_machine,
    );

    let b_on_a_detail = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws_a)),
        Some(&jar_b),
        None,
    )
    .await;
    assert_other_ws_host_get_is_403_and_omits_inventory(
        &b_on_a_detail,
        "B GET /worker-hosts/{hid} on A's path",
        &hid,
        &a_name,
        &a_hostname,
        &a_platform,
        &hardware_os,
        &software_sha,
        &a_machine,
    );

    let b_own = call(&app, "GET", &hosts_path(&ws_b), Some(&jar_b), None).await;
    assert_eq!(
        b_own.status,
        StatusCode::OK,
        "B GET /worker-hosts on B's path: {}",
        b_own.body
    );
    let b_hosts = b_own.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("B list hosts array: {}", b_own.body));
    assert!(
        b_hosts
            .iter()
            .all(|h| h["id"].as_str() != Some(hid.as_str())),
        "B GET list must not include A's hid {hid}: {}",
        b_own.body
    );

    let still_list = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_a), None).await;
    assert_eq!(
        still_list.status,
        StatusCode::OK,
        "control: A's cookie on A's GET list must still 200 (not a tautology that every host GET is 403): {}",
        still_list.body
    );
    assert!(
        still_list.body["hosts"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|h| h["id"].as_str() == Some(hid.as_str())
                && h["name"].as_str() == Some(a_name.as_str())),
        "control list must still include A's host: {}",
        still_list.body
    );

    let still_got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws_a)),
        Some(&jar_a),
        None,
    )
    .await;
    assert_eq!(
        still_got.status,
        StatusCode::OK,
        "control: A's cookie on A's GET detail must still 200: {}",
        still_got.body
    );
    assert_eq!(still_got.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(still_got.body["name"].as_str(), Some(a_name.as_str()));
}

/// catalog:list-worker-hosts-mutate-other-ws-403 — POST create /
/// pair-tokens / DELETE on another workspace is 403 not-a-member
/// and must not mint or revoke. GET list/detail is
/// list-worker-hosts-list-other-ws-403. member_can_list_but_cannot_mutate
/// is a same-workspace member (can_manage), not a foreign owner.
/// owner_can_rename_host is PATCH only. Distinct from
/// list-pair-plaintext-not-in-later-get, list-paused-host-lease-null,
/// and list-plane-runner-delete-400.
fn assert_other_ws_host_mutate_is_403_and_does_not_mint(out: &HttpOut, when: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{when} must not 200 pair/revoke: {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{when} must be 403 not-a-member: {}",
        out.raw
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("not a member"),
        "{when} should say not a member, got {err:?}: {}",
        out.raw
    );
    for key in ["pair_token", "machine_token", "install_hint"] {
        assert!(
            !json_has_key(&out.body, key),
            "{when} 403 must not include a new {key} for A: {}",
            out.body
        );
    }
    assert!(
        !out.raw.contains("flp_") && !json_contains_str(&out.body, "flp_"),
        "{when} must not mint pair_token (flp_): {}",
        out.raw
    );
    assert!(
        !out.raw.contains("flm_") && !json_contains_str(&out.body, "flm_"),
        "{when} must not mint machine_token (flm_): {}",
        out.raw
    );
}

#[tokio::test]
async fn worker_hosts_mutate_other_ws_is_403_and_does_not_mint_or_revoke() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());
    let b_attempted = format!("b-on-a-{}", Uuid::new_v4());
    let ws_a_id = Uuid::parse_str(&ws_a).expect("ws_a");

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws_a),
        Some(&jar_a),
        Some(json!({ "name": a_name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "A POST /worker-hosts: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let hid_uuid = Uuid::parse_str(&hid).expect("hid");
    assert_eq!(
        created.body["host"]["status"].as_str(),
        Some("pending"),
        "A create starts pending: {}",
        created.body
    );
    let a_pair = created.body["pair_token"]
        .as_str()
        .expect("A create pair_token")
        .to_string();
    assert!(
        a_pair.starts_with("flp_"),
        "A create pair_token prefix: {a_pair}"
    );

    let list_a = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_a), None).await;
    assert_eq!(
        list_a.status,
        StatusCode::OK,
        "A GET /worker-hosts snapshot: {}",
        list_a.body
    );
    assert!(
        !json_has_key(&list_a.body, "pair_token"),
        "A GET list must not include pair_token: {}",
        list_a.body
    );
    let a_card = list_a.body["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("A list missing created host {hid}: {}", list_a.body));
    assert!(
        a_card.get("pair_token").is_none(),
        "A list card must not include pair_token: {a_card}"
    );
    assert_eq!(
        a_card["status"].as_str(),
        Some("pending"),
        "A list snapshot still pending: {a_card}"
    );
    assert_eq!(
        a_card["name"].as_str(),
        Some(a_name.as_str()),
        "A list snapshot name: {a_card}"
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let b_create = call(
        &app,
        "POST",
        &hosts_path(&ws_a),
        Some(&jar_b),
        Some(json!({ "name": b_attempted })),
    )
    .await;
    assert_other_ws_host_mutate_is_403_and_does_not_mint(
        &b_create,
        "B POST /worker-hosts on A's path",
    );

    let b_rotate = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws_a)),
        Some(&jar_b),
        Some(json!({})),
    )
    .await;
    assert_other_ws_host_mutate_is_403_and_does_not_mint(
        &b_rotate,
        "B POST /worker-hosts/{hid}/pair-tokens on A's path",
    );

    let b_delete = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws_a)),
        Some(&jar_b),
        None,
    )
    .await;
    assert_other_ws_host_mutate_is_403_and_does_not_mint(
        &b_delete,
        "B DELETE /worker-hosts/{hid} on A's path",
    );

    let still_list = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_a), None).await;
    assert_eq!(
        still_list.status,
        StatusCode::OK,
        "A GET list after B mutate must still 200: {}",
        still_list.body
    );
    let still_hosts = still_list.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("A list hosts array: {}", still_list.body));
    let still_card = still_hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "A list must still have original hid (not revoked): {}",
                still_list.body
            )
        });
    assert_eq!(
        still_card["status"].as_str(),
        Some("pending"),
        "A host must still be pending (not revoked): {}",
        still_list.body
    );
    assert!(
        still_hosts
            .iter()
            .all(|h| h["name"].as_str() != Some(b_attempted.as_str())),
        "B's attempted create name must not appear on A's list: {}",
        still_list.body
    );

    let still_got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws_a)),
        Some(&jar_a),
        None,
    )
    .await;
    assert_eq!(
        still_got.status,
        StatusCode::OK,
        "A GET detail after B mutate must still 200: {}",
        still_got.body
    );
    assert_eq!(still_got.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(
        still_got.body["status"].as_str(),
        Some("pending"),
        "A detail must still be pending (not revoked): {}",
        still_got.body
    );
    assert_eq!(
        still_got.body["name"].as_str(),
        Some(a_name.as_str()),
        "A detail name unchanged: {}",
        still_got.body
    );

    let rows = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, ws_a_id)
        .await
        .expect("list_for_workspace A");
    assert!(
        rows.iter()
            .any(|h| h.id == hid_uuid && h.status == "pending"),
        "original hid must remain a pending worker_hosts row: {rows:?}"
    );
    assert!(
        rows.iter().all(|h| h.name != b_attempted),
        "no extra worker_hosts row with B's attempted create name {b_attempted}: {rows:?}"
    );

    let minted = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws_a)),
        Some(&jar_a),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        minted.status,
        StatusCode::OK,
        "A POST pair-tokens as A after B mutate: {}",
        minted.body
    );
    let flp = minted.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("A pair-tokens must mint a new flp_: {}", minted.body));
    assert!(flp.starts_with("flp_"), "A mint prefix: {flp}");
    assert_ne!(
        flp, a_pair,
        "A rotate must issue a new flp_ (B must not have consumed the rotate path): {}",
        minted.body
    );

    let b_own_name = format!("b-own-{}", Uuid::new_v4());
    let b_created = call(
        &app,
        "POST",
        &hosts_path(&ws_b),
        Some(&jar_b),
        Some(json!({ "name": b_own_name })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::OK,
        "control: B POST /worker-hosts on B must still 200 (not a tautology that every create 403s): {}",
        b_created.body
    );
    let b_hid = b_created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control create must return host.id: {}", b_created.body))
        .to_string();
    assert_ne!(b_hid, hid, "control host must be a different row");
    assert_eq!(
        b_created.body["host"]["name"].as_str(),
        Some(b_own_name.as_str())
    );

    let b_list = call(&app, "GET", &hosts_path(&ws_b), Some(&jar_b), None).await;
    assert_eq!(
        b_list.status,
        StatusCode::OK,
        "B GET /worker-hosts on B's path: {}",
        b_list.body
    );
    let b_hosts = b_list.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("B list hosts array: {}", b_list.body));
    assert!(
        b_hosts
            .iter()
            .all(|h| h["id"].as_str() != Some(hid.as_str())),
        "B GET list must not include A's hid {hid}: {}",
        b_list.body
    );
    assert!(
        b_hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(b_hid.as_str())),
        "B GET list must include B's own host: {}",
        b_list.body
    );
}

/// catalog:list-this-machine-only-on-loopback-origin — GET /worker-hosts
/// sets is_this_machine only when Origin/Referer is a real loopback host.
/// LAN, substring hosts, and query-string localhost must not tag the plane.
/// owner_create_and_get_list never sends Origin. Other-ws GET stays 403.
#[tokio::test]
async fn this_machine_origin_is_true_only_for_real_loopback() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);
    let pending_name = format!("pending-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": pending_name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );

    for origin in [
        "http://localhost:3030",
        "http://127.0.0.1:3030",
        "http://[::1]:3030",
    ] {
        let list =
            call_with_headers(&app, "GET", &uri, Some(&jar), &[("origin", origin)], None).await;
        assert_eq!(
            list.status,
            StatusCode::OK,
            "loopback Origin {origin} GET /worker-hosts: {}",
            list.body
        );
        assert_list_omits_pair_and_machine_tokens(&list, &pair_token, origin);
        let hosts = list_hosts(&list, origin);
        assert_list_includes_pending(hosts, &hid, &pending_name, origin);
        // A pending unpaired host has no hostname. Loopback Origin must not
        // invent a synthetic plane card and tag that instead. A live
        // this-machine extra may still be tagged.
        assert_pending_is_not_this_machine(hosts, &hid, origin);
    }

    for origin in ["http://192.168.1.50:3030", "http://10.0.0.8:3030"] {
        let list =
            call_with_headers(&app, "GET", &uri, Some(&jar), &[("origin", origin)], None).await;
        assert_eq!(
            list.status,
            StatusCode::OK,
            "LAN Origin {origin} GET /worker-hosts: {}",
            list.body
        );
        assert_list_omits_pair_and_machine_tokens(&list, &pair_token, origin);
        let hosts = list_hosts(&list, origin);
        assert_list_includes_pending(hosts, &hid, &pending_name, origin);
        assert_no_host_is_this_machine(&list, hosts, origin);
    }

    for (header, value) in [
        ("origin", "http://localhost.evil.example.test"),
        ("origin", "http://evil-127.0.0.1.example.test"),
        (
            "referer",
            "http://192.168.1.50:3030/?next=http://localhost:3030",
        ),
    ] {
        let label = format!("{header}={value}");
        let list = call_with_headers(&app, "GET", &uri, Some(&jar), &[(header, value)], None).await;
        assert_eq!(
            list.status,
            StatusCode::OK,
            "substring {label} GET /worker-hosts: {}",
            list.body
        );
        assert_list_omits_pair_and_machine_tokens(&list, &pair_token, &label);
        let hosts = list_hosts(&list, &label);
        assert_list_includes_pending(hosts, &hid, &pending_name, &label);
        assert_no_host_is_this_machine(&list, hosts, &label);
    }

    let no_origin = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        no_origin.status,
        StatusCode::OK,
        "no Origin GET /worker-hosts (same as owner_create_and_get_list): {}",
        no_origin.body
    );
    assert_list_omits_pair_and_machine_tokens(&no_origin, &pair_token, "no Origin");
    let hosts = list_hosts(&no_origin, "no Origin");
    assert_list_includes_pending(hosts, &hid, &pending_name, "no Origin");
    assert_no_host_is_this_machine(&no_origin, hosts, "no Origin");

    let (_other_email, other_jar, _other_ws) = signup_and_login(&app).await;
    let other_on_a = call(&app, "GET", &uri, Some(&other_jar), None).await;
    assert_eq!(
        other_on_a.status,
        StatusCode::FORBIDDEN,
        "other-ws GET /worker-hosts must stay 403: {}",
        other_on_a.body
    );
}

fn list_hosts<'a>(out: &'a HttpOut, what: &str) -> &'a [Value] {
    out.body["hosts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("{what}: hosts array: {}", out.body))
}

fn host_flag_true(host: &Value, key: &str) -> bool {
    host.get(key).and_then(Value::as_bool) == Some(true)
}

fn assert_list_includes_pending(hosts: &[Value], hid: &str, name: &str, what: &str) {
    assert!(
        hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(hid) && h["name"].as_str() == Some(name)),
        "{what}: list missing pending host {hid} / {name}: {hosts:?}"
    );
}

fn assert_list_omits_pair_and_machine_tokens(out: &HttpOut, pair_token: &str, what: &str) {
    for key in ["pair_token", "machine_token"] {
        assert!(
            !json_has_key(&out.body, key),
            "{what} leaked {key}: {}",
            out.body
        );
    }
    assert!(
        !out.raw.contains(pair_token) && !json_contains_str(&out.body, pair_token),
        "{what} leaked pair_token plaintext: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("flm_") && !json_contains_str(&out.body, "flm_"),
        "{what} leaked a machine token: {}",
        out.raw
    );
}

fn assert_pending_is_not_this_machine(hosts: &[Value], hid: &str, what: &str) {
    let pending = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid))
        .unwrap_or_else(|| panic!("{what}: pending {hid} missing: {hosts:?}"));
    assert!(
        !host_flag_true(pending, "is_this_machine"),
        "{what} must not tag unpaired pending {hid} as this machine: {pending}"
    );
}

fn assert_no_host_is_this_machine(out: &HttpOut, hosts: &[Value], what: &str) {
    for host in hosts {
        assert!(
            !host_flag_true(host, "is_this_machine"),
            "{what} must not set is_this_machine true (or the key must be absent): {}",
            out.body
        );
        let name = host["name"].as_str().unwrap_or("");
        assert!(
            !name.to_ascii_lowercase().contains("this machine"),
            "{what} must not grow a this-machine tag on name {name:?}: {}",
            out.body
        );
    }
    assert!(
        !out.raw.to_ascii_lowercase().contains("this machine"),
        "{what} body must not grow a this-machine tag: {}",
        out.raw
    );
}

/// catalog:list-host-create-name-optional — POST empty / whitespace /
/// omitted name is 200 pending; pair stamps the device hostname. A 10k
/// name must not persist (400, or 200 with GET name.len()<=255). Member
/// and other-ws stay 403. Control unique 1–80 still 200 with flp_.
/// Distinct from list-rename-host-patch (PATCH only).
#[tokio::test]
async fn host_create_name_optional_pair_uses_hostname() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);

    let snapshot = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        snapshot.status,
        StatusCode::OK,
        "pre-call GET /worker-hosts: {}",
        snapshot.body
    );

    let mut unnamed_token = None;
    let mut unnamed_id = None;
    for (label, body) in [
        ("empty", json!({ "name": "" })),
        ("whitespace", json!({ "name": "   " })),
        ("omitted", json!({})),
    ] {
        let out = call(&app, "POST", &uri, Some(&jar), Some(body)).await;
        assert_eq!(
            out.status,
            StatusCode::OK,
            "POST {label} name must create a pending host: {}",
            out.body
        );
        let created_name = out.body["host"]["name"].as_str().unwrap_or("?");
        assert!(
            created_name.trim().is_empty(),
            "POST {label} pending name must be blank until pair: {}",
            out.body
        );
        assert_eq!(
            out.body["host"]["status"], "pending",
            "POST {label} must be pending: {}",
            out.body
        );
        let token = out.body["pair_token"]
            .as_str()
            .expect("pair_token")
            .to_string();
        assert!(token.starts_with("flp_"), "pair_token prefix: {token}");
        unnamed_token = Some(token);
        unnamed_id = Some(
            out.body["host"]["id"]
                .as_str()
                .expect("host.id")
                .to_string(),
        );
    }

    let hostname = format!("box-{}", Uuid::new_v4());
    let pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": unnamed_token.expect("token"),
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(pair.status, StatusCode::OK, "pair unnamed: {}", pair.body);
    let hid = unnamed_id.expect("id");
    let got = call(&app, "GET", &format!("{uri}/{hid}"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "GET after pair: {}", got.body);
    assert_eq!(
        got.body["name"].as_str(),
        Some(hostname.as_str()),
        "pair must stamp the device hostname as the name: {}",
        got.body
    );
    assert_eq!(
        got.body["hostname"].as_str(),
        Some(hostname.as_str()),
        "pair hostname: {}",
        got.body
    );

    let after_optional = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        after_optional.status,
        StatusCode::OK,
        "GET after optional-name creates: {}",
        after_optional.body
    );
    let snap_count = after_optional.body["hosts"]
        .as_array()
        .expect("hosts")
        .len();

    let huge = "x".repeat(10_000);
    let huge_out = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": huge })),
    )
    .await;
    assert_ne!(
        huge_out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST 10k name must not 500: {}",
        huge_out.body
    );
    let after_huge = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        after_huge.status,
        StatusCode::OK,
        "GET after 10k name: {}",
        after_huge.body
    );
    assert!(
        !after_huge.raw.contains(&huge),
        "10k name must not persist on GET /worker-hosts: {}",
        after_huge.body
    );
    if huge_out.status == StatusCode::BAD_REQUEST {
        assert!(
            huge_out.body.get("pair_token").is_none()
                || huge_out.body["pair_token"].is_null()
                || huge_out.body["pair_token"]
                    .as_str()
                    .is_none_or(str::is_empty),
            "400 10k create must not include pair_token: {}",
            huge_out.body
        );
        let hosts = after_huge.body["hosts"].as_array().expect("hosts array");
        assert_eq!(
            hosts.len(),
            snap_count,
            "GET after 400 10k name must not add a row: {}",
            after_huge.body
        );
    } else {
        assert!(
            huge_out.status == StatusCode::OK || huge_out.status == StatusCode::CREATED,
            "POST 10k name must be 400, or 200/201 with a clamped name: {} {}",
            huge_out.status,
            huge_out.body
        );
        let created_name = huge_out.body["host"]["name"]
            .as_str()
            .unwrap_or_else(|| huge_out.body["name"].as_str().unwrap_or(""));
        assert!(
            created_name.len() <= 255,
            "200/201 10k create must clamp name.len()<=255, not persist 10k: len={} {}",
            created_name.len(),
            huge_out.body
        );
        assert_ne!(
            created_name,
            huge.as_str(),
            "200/201 must not persist the 10k string: {}",
            huge_out.body
        );
        let hid = huge_out.body["host"]["id"]
            .as_str()
            .or_else(|| huge_out.body["id"].as_str())
            .unwrap_or_else(|| panic!("clamped create must return host.id: {}", huge_out.body));
        let got = call(&app, "GET", &format!("{uri}/{hid}"), Some(&jar), None).await;
        assert_eq!(got.status, StatusCode::OK, "GET clamped host: {}", got.body);
        let stored = got.body["name"].as_str().unwrap_or("");
        assert!(
            stored.len() <= 255,
            "GET name.len() must be <=255 after 10k POST: len={} {}",
            stored.len(),
            got.body
        );
        assert_ne!(stored, huge.as_str(), "GET must not persist the 10k string");
    }

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let before_forbidden = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        before_forbidden.status,
        StatusCode::OK,
        "GET before member/other-ws: {}",
        before_forbidden.body
    );
    let before_ids: Vec<String> = before_forbidden.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    let before_count = before_ids.len();

    let member_name = format!("member-{}", Uuid::new_v4());
    let member_create = call(
        &app,
        "POST",
        &uri,
        Some(&member_jar),
        Some(json!({ "name": member_name })),
    )
    .await;
    assert_eq!(
        member_create.status,
        StatusCode::FORBIDDEN,
        "member POST unique name must stay 403: {}",
        member_create.body
    );
    assert!(
        member_create.body.get("pair_token").is_none()
            || member_create.body["pair_token"].is_null()
            || member_create.body["pair_token"]
                .as_str()
                .is_none_or(str::is_empty),
        "member 403 must not include pair_token: {}",
        member_create.body
    );

    let (_other_email, other_jar, _other_ws) = signup_and_login(&app).await;
    let other_name = format!("other-{}", Uuid::new_v4());
    let other_create = call(
        &app,
        "POST",
        &uri,
        Some(&other_jar),
        Some(json!({ "name": other_name })),
    )
    .await;
    assert_eq!(
        other_create.status,
        StatusCode::FORBIDDEN,
        "other-ws POST unique name must stay 403: {}",
        other_create.body
    );
    assert!(
        other_create.body.get("pair_token").is_none()
            || other_create.body["pair_token"].is_null()
            || other_create.body["pair_token"]
                .as_str()
                .is_none_or(str::is_empty),
        "other-ws 403 must not include pair_token: {}",
        other_create.body
    );

    let after_forbidden = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        after_forbidden.status,
        StatusCode::OK,
        "GET after member/other-ws 403: {}",
        after_forbidden.body
    );
    let after_hosts = after_forbidden.body["hosts"]
        .as_array()
        .expect("hosts array");
    let after_ids: Vec<String> = after_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        after_hosts.len(),
        before_count,
        "member/other-ws POST must not insert: {}",
        after_forbidden.body
    );
    assert_eq!(
        after_ids, before_ids,
        "member/other-ws POST must not add a host id: {after_ids:?}"
    );
    assert!(
        after_hosts
            .iter()
            .all(|h| h["name"].as_str() != Some(member_name.as_str())
                && h["name"].as_str() != Some(other_name.as_str())),
        "member/other-ws unique names must not appear on GET: {}",
        after_forbidden.body
    );

    let ok_name = format!("ok-{}", Uuid::new_v4());
    assert!(
        (1..=80).contains(&ok_name.len()),
        "control name must be 1–80: {ok_name:?}"
    );
    let ok = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": ok_name })),
    )
    .await;
    assert_eq!(
        ok.status,
        StatusCode::OK,
        "control unique 1–80 name must still 200: {}",
        ok.body
    );
    assert_eq!(
        ok.body["host"]["name"].as_str(),
        Some(ok_name.as_str()),
        "control create must keep that name: {}",
        ok.body
    );
    let pair_token = ok.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control create must return pair_token: {}", ok.body));
    assert!(
        pair_token.starts_with("flp_"),
        "control pair_token prefix: {pair_token}"
    );

    let listed = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "control GET: {}",
        listed.body
    );
    assert!(
        listed.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|h| h["name"].as_str() == Some(ok_name.as_str())),
        "control name must appear on GET /worker-hosts: {}",
        listed.body
    );
}

/// Catalog: Cancel discards the unused worker (DELETE → revoke). Revoke of a
/// never-paired leftover stays hidden on GET list. Must not appear in
/// `list_assignable` or be handed out by `/me/lease`. Other-ws cancel is
/// 403/404 and must not change A's row. A paired-then-removed box stays
/// listed (Show removed) and a live paired+heartbeat host is the listed +
/// assignable control so this is not a tautology that every host is hidden.
#[tokio::test]
async fn cancel_unused_pending_http_discards_never_paired_hidden_not_assignable() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let leftover_name = format!("never-paired-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": leftover_name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(created.body["host"]["status"], "pending");
    assert!(
        created.body["host"]["paired_at"].is_null(),
        "unused create is never-paired: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(|| panic!("host.id: {}", created.body));
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let hosts = listed.body["hosts"].as_array().expect("hosts");
    // Product list: unused pending stays visible until Cancel (Done keeps it).
    assert!(
        hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(&hid.to_string()) && h["status"] == "pending"),
        "unused pending must be on the list until Cancel: {}",
        listed.body
    );
    let assignable_before = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !assignable_before.contains(&hid),
        "pending must not appear in list_assignable: {assignable_before:?}"
    );

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    let other_path = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&other_ws)),
        Some(&other_jar),
        None,
    )
    .await;
    assert!(
        other_path.status == StatusCode::FORBIDDEN || other_path.status == StatusCode::NOT_FOUND,
        "other-ws path cancel must be 403/404: {} {}",
        other_path.status,
        other_path.body
    );
    let other_member = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&other_jar),
        None,
    )
    .await;
    assert!(
        other_member.status == StatusCode::FORBIDDEN
            || other_member.status == StatusCode::NOT_FOUND,
        "other-ws cookie on A's path must be 403/404: {} {}",
        other_member.status,
        other_member.body
    );

    let still = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "A's unused pending must survive other-ws cancel: {}",
        still.body
    );
    assert_eq!(
        still.body["status"], "pending",
        "other-ws cancel must not revoke A's row: {}",
        still.body
    );
    assert!(
        still.body["paired_at"].is_null(),
        "other-ws cancel must not pair A's leftover: {}",
        still.body
    );
    let listed_after_other = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_after_other.status,
        StatusCode::OK,
        "list after other-ws cancel: {}",
        listed_after_other.body
    );
    assert!(
        listed_after_other.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|h| h["id"].as_str() == Some(&hid.to_string()) && h["status"] == "pending"),
        "A's unused pending must stay listed after other-ws cancel: {}",
        listed_after_other.body
    );

    let revoked = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        revoked.status,
        StatusCode::OK,
        "Cancel/DELETE unused pending must revoke: {}",
        revoked.body
    );
    assert_eq!(
        revoked.body["status"], "disabled",
        "revoke leftover: {}",
        revoked.body
    );
    assert!(
        revoked.body["paired_at"].is_null(),
        "Cancel leftover stays never-paired: {}",
        revoked.body
    );

    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let stored = fleet_db::repos::worker_hosts::get(&state.db, hid)
        .await
        .expect("get leftover")
        .unwrap_or_else(|| {
            panic!("Cancel leftover row must remain after revoke (not hard-deleted)")
        });
    assert_eq!(
        stored.status,
        fleet_db::WORKER_HOST_STATUS_DISABLED,
        "revoke persists disabled: {stored:?}"
    );
    assert!(
        stored.paired_at.is_none(),
        "persisted leftover stays never-paired: {stored:?}"
    );
    assert!(
        stored.machine_token_hash.is_none(),
        "revoke must clear machine_token_hash: {stored:?}"
    );
    assert!(
        stored.last_seen_at.is_none(),
        "unused leftover has no last_seen: {stored:?}"
    );
    assert!(
        stored
            .hostname
            .as_deref()
            .is_none_or(|s| s.trim().is_empty()),
        "unused leftover has no hostname: {stored:?}"
    );
    assert!(
        stored
            .platform
            .as_deref()
            .is_none_or(|s| s.trim().is_empty()),
        "unused leftover has no platform: {stored:?}"
    );
    let repo_listed = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, ws_id)
        .await
        .expect("list_for_workspace");
    assert!(
        repo_listed.iter().any(|h| h.id == hid),
        "revoke keeps the leftover row; GET list hides it: {:?}",
        repo_listed.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "list after cancel: {}",
        after.body
    );
    let after_hosts = after.body["hosts"].as_array().expect("hosts");
    assert_list_has_no_usable_leftover(
        after_hosts,
        hid,
        leftover_name.as_str(),
        after.body.clone(),
    );

    let assignable_after = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !assignable_after.contains(&hid),
        "Cancel leftover must not appear in list_assignable: {assignable_after:?}"
    );

    let pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": leftover_name,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_ne!(
        pair.status,
        StatusCode::OK,
        "discarded unused pair token must not consume: {}",
        pair.body
    );

    let leftover_claim = enqueue_spawn_for_host(&state, ws_id, hid).await;
    let burned_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &pair_token,
        Some(json!({})),
    )
    .await;
    assert_ne!(
        burned_lease.status,
        StatusCode::OK,
        "discarded pair token must not /me/lease: {}",
        burned_lease.body
    );
    assert_lease_does_not_hand_out_hid(&burned_lease, hid, leftover_claim, "pair_token /me/lease");
    assert_claim_unleased(
        &state,
        leftover_claim,
        "leftover spawn after burned pair_token lease",
    )
    .await;

    let (live_id, live_machine) = create_pair_and_heartbeat(
        &app,
        &jar,
        &ws,
        &format!("live-{}", Uuid::new_v4()),
        true,
        false,
    )
    .await;
    let listed_live = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_live.status,
        StatusCode::OK,
        "list after live pair: {}",
        listed_live.body
    );
    let live_hosts = listed_live.body["hosts"].as_array().expect("hosts");
    assert!(
        live_hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(&live_id.to_string())),
        "control: paired+heartbeat host must stay listed: {}",
        listed_live.body
    );
    assert_list_has_no_usable_leftover(
        live_hosts,
        hid,
        leftover_name.as_str(),
        listed_live.body.clone(),
    );
    let assignable_live = worker_hosts_assignable(&state, &ws).await;
    assert!(
        assignable_live.contains(&live_id),
        "control: paired+heartbeat host must be in list_assignable: {assignable_live:?}"
    );
    assert!(
        !assignable_live.contains(&hid),
        "Cancel leftover must stay out of list_assignable next to a live host: {assignable_live:?}"
    );

    let live_miss = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &live_machine,
        Some(json!({})),
    )
    .await;
    assert_lease_does_not_hand_out_hid(
        &live_miss,
        hid,
        leftover_claim,
        "live /me/lease must not take leftover hid",
    );
    assert_claim_unleased(
        &state,
        leftover_claim,
        "leftover spawn after live /me/lease",
    )
    .await;

    let live_claim = enqueue_spawn_for_host(&state, ws_id, live_id).await;
    let live_hit = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &live_machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        live_hit.status,
        StatusCode::OK,
        "control live /me/lease: {}",
        live_hit.body
    );
    assert_eq!(
        live_hit.body["claim"]["id"].as_str(),
        Some(live_claim.to_string().as_str()),
        "control: live paired+heartbeat /me/lease must hand out its own claim: {}",
        live_hit.body
    );
    assert_lease_does_not_hand_out_hid(
        &live_hit,
        hid,
        leftover_claim,
        "control live lease must not be leftover hid",
    );
    assert_claim_unleased(
        &state,
        leftover_claim,
        "leftover spawn after live claimed its own work",
    )
    .await;
    let live_token = live_hit.body["claim"]["lease_token"]
        .as_str()
        .expect("live lease_token")
        .to_string();
    let live_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{live_claim}/complete"),
        &live_machine,
        Some(json!({ "lease_token": live_token, "ok": true })),
    )
    .await;
    assert_eq!(
        live_done.status,
        StatusCode::OK,
        "complete live control claim: {}",
        live_done.body
    );

    let removed = call(
        &app,
        "DELETE",
        &format!("{}/{live_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        removed.status,
        StatusCode::OK,
        "revoke paired host: {}",
        removed.body
    );
    let listed_removed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_removed.status,
        StatusCode::OK,
        "list after paired revoke: {}",
        listed_removed.body
    );
    let removed_hosts = listed_removed.body["hosts"].as_array().expect("hosts");
    assert!(
        removed_hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(&live_id.to_string())),
        "revoke of a paired box stays listed (Show removed): {}",
        listed_removed.body
    );
    assert_list_has_no_usable_leftover(
        removed_hosts,
        hid,
        leftover_name.as_str(),
        listed_removed.body.clone(),
    );
    let assignable_final = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !assignable_final.contains(&hid) && !assignable_final.contains(&live_id),
        "neither leftover nor removed paired box may be assignable: {assignable_final:?}"
    );
}

fn assert_list_has_no_usable_leftover(
    hosts: &[Value],
    hid: Uuid,
    leftover_name: &str,
    body: Value,
) {
    let hid_s = hid.to_string();
    assert!(
        hosts.iter().all(|h| {
            let same = h["id"].as_str() == Some(hid_s.as_str())
                || h["name"].as_str() == Some(leftover_name);
            !same || h["status"] == "disabled"
        }),
        "revoked leftover must not stay usable on GET list: {body}"
    );
}

fn assert_lease_does_not_hand_out_hid(
    lease: &HttpOut,
    hid: Uuid,
    leftover_claim: Uuid,
    what: &str,
) {
    let dumped = lease.body.to_string();
    let hid_s = hid.to_string();
    let leftover_s = leftover_claim.to_string();
    if let Some(claim) = lease.body.get("claim").filter(|c| !c.is_null()) {
        assert_ne!(
            claim["id"].as_str(),
            Some(leftover_s.as_str()),
            "{what} must not lease leftover claim {leftover_s}: {}",
            lease.body
        );
        let payload = &claim["payload"];
        assert_ne!(
            payload["host_id"].as_str(),
            Some(hid_s.as_str()),
            "{what} must not hand out leftover hid {hid_s}: {}",
            lease.body
        );
    }
    if lease.status != StatusCode::OK {
        assert!(
            !dumped.contains(&leftover_s),
            "{what} error must not include leftover claim: {}",
            lease.body
        );
    }
}

/// Catalog: Done keeps the unused pending worker (no DELETE). The pair
/// panel close is UI-only — GET list / GET detail / reload must still
/// show the same pending card. Distinct from
/// `cancel_unused_pending_http_discards_never_paired_hidden_not_assignable`.
#[tokio::test]
async fn done_keeps_unused_pending_host_after_create_without_delete() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("done-keeps-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(
        created.body["host"]["status"], "pending",
        "Add worker creates pending: {}",
        created.body
    );
    assert!(
        created.body["host"]["paired_at"].is_null(),
        "unused create is never-paired: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(|| panic!("host.id: {}", created.body));
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );

    let assert_still_pending = |label: &str, body: &Value, hid: Uuid, name: &str| {
        assert_eq!(body["status"], "pending", "{label} status: {body}");
        assert_eq!(
            body["id"].as_str(),
            Some(hid.to_string().as_str()),
            "{label} id"
        );
        assert_eq!(body["name"].as_str(), Some(name), "{label} name");
        assert!(
            body["paired_at"].is_null(),
            "{label} must stay never-paired: {body}"
        );
        assert_ne!(
            body["status"], "disabled",
            "{label} must not be revoked: {body}"
        );
        assert!(
            body.get("pair_token").is_none(),
            "{label} must not re-echo pair_token: {body}"
        );
    };

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let hosts = listed.body["hosts"].as_array().expect("hosts");
    let row = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(&hid.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "unused pending must stay on the list after Done: {}",
                listed.body
            )
        });
    assert_still_pending("list", row, hid, &name);

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
    assert_still_pending("get", &got.body, hid, &name);

    // Reload (second GET list + GET detail) — Done must not have revoked.
    let reload = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        reload.status,
        StatusCode::OK,
        "reload list: {}",
        reload.body
    );
    let reload_hosts = reload.body["hosts"].as_array().expect("hosts");
    let reload_row = reload_hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(&hid.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "card must stay pending after reload (Done does not DELETE): {}",
                reload.body
            )
        });
    assert_still_pending("reload list", reload_row, hid, &name);
    let reload_got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        reload_got.status,
        StatusCode::OK,
        "reload get: {}",
        reload_got.body
    );
    assert_still_pending("reload get", &reload_got.body, hid, &name);

    let stored = fleet_db::repos::worker_hosts::get(&state.db, hid)
        .await
        .expect("get pending")
        .unwrap_or_else(|| panic!("Done must keep the unused pending row (not hard-deleted)"));
    assert_eq!(
        stored.status, "pending",
        "Done must not persist disabled: {stored:?}"
    );
    assert!(
        stored.paired_at.is_none(),
        "Done host stays never-paired: {stored:?}"
    );
    assert_ne!(
        stored.status,
        fleet_db::WORKER_HOST_STATUS_DISABLED,
        "Done must not revoke: {stored:?}"
    );

    let assignable = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !assignable.contains(&hid),
        "unused pending must not appear in list_assignable: {assignable:?}"
    );

    // Original install token still works — Done did not discard it.
    let pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": format!("later-{name}"),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        pair.status,
        StatusCode::OK,
        "Done must leave the unused pair token consumable (Cancel burns it): {}",
        pair.body
    );
}

/// POST create `install_hint` and pair `control_plane_url` embed
/// WORKER_PUBLIC_BASE via `created_payload` / `worker_api_base` (not a
/// hand-built hint). OAuth stays on API_PUBLIC_BASE. Unset WORKER may
/// fall back to the API origin. Pair uses the same `paired_payload` as
/// bootstrap.
#[tokio::test]
async fn install_hint_worker_public_base_not_api_public_base() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let (app, state) = app_with_state_opts(None, Some(API)).await;
    assert_eq!(
        state.integrations.api_public_base, API,
        "Connect/OAuth must stay on API_PUBLIC_BASE, not WORKER_PUBLIC_BASE"
    );
    assert_eq!(
        state.integrations.callback_url("github"),
        format!("{API}/api/integrations/oauth/callback/github")
    );
    assert!(
        !state.integrations.callback_url("github").contains(WORKER),
        "OAuth callback must not flip onto WORKER_PUBLIC_BASE"
    );

    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "studio" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();
    let hint = created.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("create install_hint: {}", created.body));
    assert_eq!(
        hint,
        format!(
            "curl -fsSL {WORKER}/install-worker.sh | FLEET_URL={WORKER} FLEET_PAIR_TOKEN={pair_token} sh"
        ),
        "install_hint must come from created_payload/worker_api_base (WORKER_PUBLIC_BASE)"
    );
    assert!(
        !hint.contains(API),
        "install_hint must not embed API_PUBLIC_BASE: {hint}"
    );
    assert!(
        !created.raw.contains(API),
        "create body must not embed API_PUBLIC_BASE"
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "studio",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    assert_eq!(
        paired.body["control_plane_url"].as_str(),
        Some(WORKER),
        "pair control_plane_url must be WORKER_PUBLIC_BASE: {}",
        paired.body
    );
    assert_ne!(
        paired.body["control_plane_url"].as_str(),
        Some(API),
        "pair control_plane_url must not be API_PUBLIC_BASE"
    );
    assert!(
        !paired.raw.contains(API),
        "pair body must not embed API_PUBLIC_BASE"
    );

    let integ = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(integ.status, StatusCode::OK, "integrations: {}", integ.body);
    let items = integ.body["integrations"]
        .as_array()
        .unwrap_or_else(|| panic!("integrations array: {}", integ.body));
    let github = items
        .iter()
        .find(|i| i["id"] == "github")
        .unwrap_or_else(|| panic!("github missing: {}", integ.body));
    let cb = github["oauth_callback_url"]
        .as_str()
        .unwrap_or_else(|| panic!("oauth_callback_url: {github}"));
    assert_eq!(cb, format!("{API}/api/integrations/oauth/callback/github"));
    assert!(
        !cb.contains(WORKER),
        "Connect/OAuth must not flip onto WORKER_PUBLIC_BASE: {cb}"
    );

    env.set_worker_public_base(None);
    let fallback = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "fallback" })),
    )
    .await;
    assert_eq!(
        fallback.status,
        StatusCode::OK,
        "create after unset WORKER_PUBLIC_BASE: {}",
        fallback.body
    );
    let fallback_token = fallback.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("fallback pair_token: {}", fallback.body));
    let fallback_hint = fallback.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("fallback install_hint: {}", fallback.body));
    assert_eq!(
        fallback_hint,
        format!(
            "curl -fsSL {API}/install-worker.sh | FLEET_URL={API} FLEET_PAIR_TOKEN={fallback_token} sh"
        ),
        "unset WORKER_PUBLIC_BASE may embed API_PUBLIC_BASE"
    );
    assert!(
        !fallback_hint.contains(WORKER),
        "fallback install_hint must not keep WORKER_PUBLIC_BASE: {fallback_hint}"
    );

    let fallback_pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": fallback_token,
            "hostname": "fallback",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        fallback_pair.status,
        StatusCode::OK,
        "fallback pair: {}",
        fallback_pair.body
    );
    assert_eq!(
        fallback_pair.body["control_plane_url"].as_str(),
        Some(API),
        "unset WORKER_PUBLIC_BASE pair URL falls back to API_PUBLIC_BASE: {}",
        fallback_pair.body
    );
}

/// catalog:list-pair-control-plane-url-ignores-host-header — create
/// `install_hint`, pair/bootstrap `control_plane_url`, and served install
/// scripts use WORKER_PUBLIC_BASE (fallback API_PUBLIC_BASE), never request
/// Host / X-Forwarded-Host. Distinct from
/// [`install_hint_worker_public_base_not_api_public_base`] (env preference,
/// no Host), list-oauth-start-callback-ignores-host-header (Connect
/// `redirect_uri`), list-loopback-pair-warning-and-offbox, and
/// list-this-machine-only-on-loopback-origin (`is_this_machine`).
const PAIR_HOST_EVIL: &str = "evil.example.test";

fn pair_spoofed_host_headers() -> [(&'static str, &'static str); 3] {
    [
        ("Host", PAIR_HOST_EVIL),
        ("X-Forwarded-Host", PAIR_HOST_EVIL),
        ("X-Forwarded-Proto", "https"),
    ]
}

fn expected_worker_install_hint(base: &str, pair_token: &str) -> String {
    format!(
        "curl -fsSL {base}/install-worker.sh | FLEET_URL={base} FLEET_PAIR_TOKEN={pair_token} sh"
    )
}

fn assert_omits_evil_pair_origin(raw: &str, label: &str) {
    let lower = raw.to_ascii_lowercase();
    for needle in [
        PAIR_HOST_EVIL,
        "https://evil.example.test",
        "http://evil.example.test",
        "https%3a%2f%2fevil.example.test",
        "http%3a%2f%2fevil.example.test",
    ] {
        assert!(
            !lower.contains(needle),
            "{label} must not mint install/pair URL from Host/X-Forwarded-Host ({needle}): {raw}"
        );
    }
}

#[tokio::test]
async fn pair_ignores_host_header_for_control_plane_url() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);
    assert!(
        !WORKER.contains(PAIR_HOST_EVIL) && !API.contains(PAIR_HOST_EVIL),
        "configured bases must not already be the spoofed Host"
    );

    let _env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let (app, state) = app_with_state_opts(None, Some(API)).await;
    assert_eq!(
        state.integrations.api_public_base, API,
        "Connect/OAuth must stay on API_PUBLIC_BASE, not WORKER_PUBLIC_BASE or Host"
    );

    let (_email, jar, ws) = signup_and_login(&app).await;
    let spoofed = pair_spoofed_host_headers();

    let health = call_with_headers(&app, "GET", "/health", None, &spoofed, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health with Host: {PAIR_HOST_EVIL} must stay 200 so this is not a tautology that Host 400s: {}",
        health.body
    );

    let cookie_pair = call_with_headers(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        &spoofed,
        Some(json!({
            "token": "",
            "hostname": format!("cookie-only-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(&cookie_pair, "Cookie-only /pair with spoofed Host", false);
    assert_omits_evil_pair_origin(&cookie_pair.raw, "Cookie-only /pair with spoofed Host");

    let cookie_pair_no_host = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        Some(json!({
            "token": "",
            "hostname": format!("cookie-only-nohost-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(
        &cookie_pair_no_host,
        "Cookie-only /pair without Host (control)",
        false,
    );

    let create_name = format!("pair-host-{}", Uuid::new_v4());
    let created = call_with_headers(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        &spoofed,
        Some(json!({ "name": create_name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create with Host: {}",
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();
    assert!(
        pair_token.starts_with("flp_") && pair_token.len() > 8,
        "snapshot flp_ must be a real pair token: {pair_token}"
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("create host.id: {}", created.body))
        .to_string();
    let hint = created.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("create install_hint: {}", created.body));
    assert_eq!(
        hint,
        expected_worker_install_hint(WORKER, &pair_token),
        "install_hint must stay WORKER_PUBLIC_BASE under spoofed Host"
    );
    assert_omits_evil_pair_origin(&created.raw, "POST /worker-hosts with spoofed Host");

    let pair_hn = format!("pair-hn-{}", Uuid::new_v4());
    let paired = call_with_headers(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        &spoofed,
        Some(pair_body(&pair_token, &pair_hn)),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "pair with Host: {}",
        paired.body
    );
    assert_eq!(
        paired.body["control_plane_url"].as_str(),
        Some(WORKER),
        "pair control_plane_url must be WORKER_PUBLIC_BASE, not Host: {}",
        paired.body
    );
    assert_ne!(
        paired.body["control_plane_url"].as_str(),
        Some("https://evil.example.test"),
        "pair control_plane_url must not be the spoofed origin"
    );
    assert_omits_evil_pair_origin(&paired.raw, "POST /pair with spoofed Host");
    let machine = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair machine_token: {}", paired.body))
        .to_string();
    assert!(
        machine.starts_with("flm_") && machine.len() > 8,
        "pair must mint flm_: {machine}"
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "later GET: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()));
    for key in ["control_plane_url", "machine_token", "pair_token"] {
        assert!(
            !json_has_key(&got.body, key),
            "later GET /worker-hosts/{{hid}} must omit {key}: {}",
            got.body
        );
    }
    assert_later_read_omits_pair_plaintext(&got, &[&pair_token]);
    assert_later_http_omits_machine_plaintext(
        &got,
        &[&machine],
        "later GET /worker-hosts/{hid} after pair with spoofed Host",
    );

    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let boot_hn = format!("boot-hn-{}", Uuid::new_v4());
    let boot = call_inner_headers(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        None,
        Some(TEST_BOOTSTRAP_SECRET),
        &spoofed,
        Some(json!({
            "hostname": boot_hn,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert!(
        boot.status == StatusCode::OK || boot.status == StatusCode::CONFLICT,
        "bootstrap with spoofed Host: {} {}",
        boot.status,
        boot.body
    );
    if boot.status == StatusCode::CONFLICT {
        let err = boot.body["error"].as_str().unwrap_or("");
        assert!(
            err.contains("no_workspace"),
            "bootstrap 409 must be no_workspace, not a Host-derived mint: {}",
            boot.body
        );
    } else {
        assert_eq!(
            boot.body["control_plane_url"].as_str(),
            Some(WORKER),
            "bootstrap control_plane_url must be WORKER_PUBLIC_BASE, not Host: {}",
            boot.body
        );
        assert_ne!(
            boot.body["control_plane_url"].as_str(),
            Some("https://evil.example.test"),
            "bootstrap control_plane_url must not be the spoofed origin"
        );
    }
    assert_omits_evil_pair_origin(&boot.raw, "POST /bootstrap with spoofed Host");

    for path in ["/install-worker.sh", "/install-worker.ps1"] {
        let script = call_with_headers(&app, "GET", path, None, &spoofed, None).await;
        assert_eq!(
            script.status,
            StatusCode::OK,
            "{path} with spoofed Host: {}",
            script.raw
        );
        assert!(
            !script.raw.is_empty(),
            "{path} must still serve a body under spoofed Host"
        );
        assert_omits_evil_pair_origin(&script.raw, &format!("GET {path} with spoofed Host"));
    }

    // Control: no Host still matches install_hint_worker_public_base_not_api_public_base.
    let control_name = format!("pair-nohost-{}", Uuid::new_v4());
    let control = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": control_name })),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "no-Host create: {}",
        control.body
    );
    let control_token = control.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no-Host pair_token: {}", control.body))
        .to_string();
    let control_hint = control.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("no-Host install_hint: {}", control.body));
    assert_eq!(
        control_hint,
        expected_worker_install_hint(WORKER, &control_token),
        "no-Host create must still match install_hint_worker_public_base_not_api_public_base"
    );
    assert!(
        !control_hint.contains(API),
        "no-Host install_hint must not embed API_PUBLIC_BASE: {control_hint}"
    );

    let control_pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(
            &control_token,
            &format!("nohost-{}", Uuid::new_v4()),
        )),
    )
    .await;
    assert_eq!(
        control_pair.status,
        StatusCode::OK,
        "no-Host pair: {}",
        control_pair.body
    );
    assert_eq!(
        control_pair.body["control_plane_url"].as_str(),
        Some(WORKER),
        "no-Host pair control_plane_url must be WORKER_PUBLIC_BASE: {}",
        control_pair.body
    );
    assert_ne!(
        control_pair.body["control_plane_url"].as_str(),
        Some(API),
        "no-Host pair control_plane_url must not be API_PUBLIC_BASE"
    );
    assert!(
        !control_pair.raw.contains(API),
        "no-Host pair body must not embed API_PUBLIC_BASE"
    );
}

/// Pair token plaintext is returned once on create (and once on rotate).
/// Later GET/list — including the control-plane card and list envelope —
/// must omit the keys and the plaintext string.
#[tokio::test]
async fn pair_token_plaintext_only_on_create_not_later_get_or_list() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "studio" })),
    )
    .await;
    let pair_token = assert_create_or_rotate_pair_once(&created, None);
    let created_host = created
        .body
        .get("host")
        .unwrap_or_else(|| panic!("create must return host object: {}", created.body));
    let hid = created_host["id"].as_str().expect("host.id").to_string();

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()));
    assert_later_read_omits_pair_plaintext(&got, &[&pair_token]);

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    assert_later_read_omits_pair_plaintext(&list, &[&pair_token]);
    let hosts = list.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("hosts array: {}", list.body));
    let listed = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("list missing created host: {}", list.body));
    assert_host_object_omits_pair_secrets(listed, &pair_token);

    let rotated = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    let new_token = assert_create_or_rotate_pair_once(&rotated, Some(&pair_token));

    let got_after = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got_after.status,
        StatusCode::OK,
        "get after rotate: {}",
        got_after.body
    );
    assert_later_read_omits_pair_plaintext(&got_after, &[&pair_token, &new_token]);

    let list_after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list_after.status,
        StatusCode::OK,
        "list after rotate: {}",
        list_after.body
    );
    assert_later_read_omits_pair_plaintext(&list_after, &[&pair_token, &new_token]);
    let hosts_after = list_after.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("hosts array after rotate: {}", list_after.body));
    let listed_after = hosts_after
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("list missing host after rotate: {}", list_after.body));
    assert_host_object_omits_pair_secrets(listed_after, &pair_token);
    assert_host_object_omits_pair_secrets(listed_after, &new_token);
}

/// catalog:list-machine-token-plaintext-not-in-later-get — POST /pair
/// (and bootstrap rotate) return `machine_token` (`flm_…`) once. Later
/// GET/list/heartbeat/lease must omit that plaintext anywhere (host object,
/// envelope extras, install_hint) and have no `machine_token` /
/// `machine_token_hash` key. Distinct from pair_token_plaintext (flp_) and
/// cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease.
#[tokio::test]
async fn machine_token_plaintext_only_on_pair_or_bootstrap_not_later_get_heartbeat_or_lease() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("mtok-{}", Uuid::new_v4());

    let (host_id, old_flm) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    assert!(
        old_flm.starts_with("flm_") && old_flm.len() > 8,
        "POST /pair must return machine_token flm_… once (control, not a tautology that every host JSON omits a token): {old_flm}"
    );
    let hid = host_id.to_string();
    let hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 0,
        "max_concurrent": 2
    });

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after pair: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()));
    assert_later_http_omits_machine_plaintext(
        &got,
        &[&old_flm],
        "GET /worker-hosts/{id} after pair",
    );

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "list after pair: {}",
        list.body
    );
    let listed = list_host_by_id(&list, &hid, "list after pair");
    assert_eq!(listed["id"].as_str(), Some(hid.as_str()));
    assert_later_http_omits_machine_plaintext(&list, &[&old_flm], "GET list after pair");

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &old_flm,
        Some(hb_body.clone()),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat after pair: {}",
        hb.body
    );
    assert_later_http_omits_machine_plaintext(&hb, &[&old_flm], "POST /me/heartbeat after pair");
    if let Some(host) = hb.body.get("host") {
        assert!(
            !json_has_key(host, "machine_token") && !json_has_key(host, "machine_token_hash"),
            "heartbeat host object leaked machine_token key: {host}"
        );
        assert!(
            !json_contains_str(host, &old_flm),
            "heartbeat host object leaked machine token plaintext: {host}"
        );
    }

    let lease = call_bearer(&app, "POST", "/api/worker-hosts/me/lease", &old_flm, None).await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "lease after pair: {}",
        lease.body
    );
    assert_later_http_omits_machine_plaintext(&lease, &[&old_flm], "POST /me/lease after pair");

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(hb_body.clone()),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_hb, "Cookie-only /me/heartbeat");
    assert!(
        !cookie_hb.raw.contains(&old_flm) && !json_contains_str(&cookie_hb.body, &old_flm),
        "cookie-only /me/heartbeat must not echo machine token plaintext: {}",
        cookie_hb.raw
    );

    let machine_id = got.body["machine_id"]
        .as_str()
        .unwrap_or(hid.as_str())
        .to_string();
    let boot_secret = std::env::var("FLEET_BOOTSTRAP_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| TEST_BOOTSTRAP_SECRET.to_string());
    let boot = {
        let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(&boot_secret)).await;
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            &boot_secret,
            Some(json!({
                "hostname": name,
                "platform": "linux-x86_64",
                "advertised_host": true,
                "advertised_container": false,
                "machine_id": machine_id,
                "workspace_id": ws
            })),
        )
        .await
    };
    assert_eq!(
        boot.status,
        StatusCode::OK,
        "bootstrap rotate: {}",
        boot.body
    );
    let new_flm = boot.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "POST /bootstrap rotate must return machine_token once: {}",
                boot.body
            )
        })
        .to_string();
    assert!(
        new_flm.starts_with("flm_") && new_flm.len() > 8,
        "bootstrap rotate machine_token must be flm_…: {new_flm}"
    );
    assert_ne!(
        new_flm, old_flm,
        "bootstrap rotate must mint a new machine token"
    );
    assert_eq!(
        boot.body["host_id"].as_str(),
        Some(hid.as_str()),
        "bootstrap rotate must keep the same host: {}",
        boot.body
    );
    assert!(
        boot.raw.contains(&new_flm),
        "bootstrap rotate HTTP body must include the new machine_token"
    );
    assert!(
        !boot.raw.contains(&old_flm),
        "bootstrap rotate must not echo the old machine_token: {}",
        boot.raw
    );

    let got_after = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got_after.status,
        StatusCode::OK,
        "get after rotate: {}",
        got_after.body
    );
    assert_later_http_omits_machine_plaintext(
        &got_after,
        &[&old_flm, &new_flm],
        "GET /worker-hosts/{id} after bootstrap rotate",
    );

    let list_after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list_after.status,
        StatusCode::OK,
        "list after rotate: {}",
        list_after.body
    );
    let _ = list_host_by_id(&list_after, &hid, "list after rotate");
    assert_later_http_omits_machine_plaintext(
        &list_after,
        &[&old_flm, &new_flm],
        "GET list after bootstrap rotate",
    );

    let hb_new = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &new_flm,
        Some(hb_body.clone()),
    )
    .await;
    assert_eq!(
        hb_new.status,
        StatusCode::OK,
        "heartbeat with rotated token: {}",
        hb_new.body
    );
    assert_later_http_omits_machine_plaintext(
        &hb_new,
        &[&old_flm, &new_flm],
        "POST /me/heartbeat after bootstrap rotate",
    );

    let old_hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &old_flm,
        Some(hb_body),
    )
    .await;
    assert_eq!(
        old_hb.status,
        StatusCode::UNAUTHORIZED,
        "old flm_ /me/heartbeat after rotate must 401: {}",
        old_hb.body
    );
    assert_ne!(
        old_hb.status,
        StatusCode::OK,
        "old flm_ must not still heartbeat after rotate: {}",
        old_hb.body
    );
}

/// catalog:unit-patch-name — owner PATCH name persists (GET + list).
#[tokio::test]
async fn owner_can_rename_host() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "studio" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(created.body["host"]["name"], "studio");
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let isolation = created.body["host"]["isolation"].clone();
    let gen0 = created.body["host"]["settings_gen"]
        .as_i64()
        .expect("settings_gen");

    let denied = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        None,
        Some(json!({ "name": "stolen" })),
    )
    .await;
    assert_eq!(
        denied.status,
        StatusCode::UNAUTHORIZED,
        "unauthenticated PATCH name: {}",
        denied.body
    );

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    let foreign_ws = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(json!({ "name": "foreign" })),
    )
    .await;
    assert_eq!(
        foreign_ws.status,
        StatusCode::NOT_FOUND,
        "other workspace PATCH name: {}",
        foreign_ws.body
    );
    let foreign_member = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&other_jar),
        Some(json!({ "name": "foreign" })),
    )
    .await;
    assert_eq!(
        foreign_member.status,
        StatusCode::FORBIDDEN,
        "non-member PATCH name: {}",
        foreign_member.body
    );

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "name": "  renamed-studio  " })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "patch: {}", patched.body);
    assert_eq!(
        patched.body["name"], "renamed-studio",
        "PATCH name trims: {}",
        patched.body
    );
    assert_eq!(
        patched.body["isolation"], isolation,
        "rename must not change isolation: {}",
        patched.body
    );
    let gen1 = patched.body["settings_gen"]
        .as_i64()
        .expect("patched settings_gen");
    assert_eq!(
        gen1,
        gen0 + 1,
        "name is a CP setting so rename bumps gen: {}",
        patched.body
    );

    let blank = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_ne!(
        blank.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "blank PATCH name must not 500: {}",
        blank.body
    );
    assert_eq!(
        blank.status,
        StatusCode::BAD_REQUEST,
        "blank PATCH name must be 400, not a no-op 200: {}",
        blank.body
    );
    let blank_err = blank.body["error"].as_str().unwrap_or("");
    assert!(
        blank_err.contains("required"),
        "blank PATCH name error: {}",
        blank.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
    assert_eq!(got.body["name"], "renamed-studio");
    assert_eq!(got.body["settings_gen"].as_i64().expect("get gen"), gen1);

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    let hosts = list.body["hosts"].as_array().expect("hosts array");
    let host = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("list missing renamed host: {}", list.body));
    assert_eq!(host["name"], "renamed-studio", "list name: {}", list.body);
}

/// Drive the oneshot raw body: a 404/403 must not name A's workspace or
/// host secrets. Not a status-only helper — StatusCode is asserted at
/// the call site so `-- host_foreign_id` fails on the leak, not only 404.
fn assert_oneshot_raw_omits_a_host_tenant(
    out: &HttpOut,
    what: &str,
    ws_a: &str,
    name: &str,
    hostname: &str,
    machine_token: &str,
) {
    for (label, needle) in [
        ("workspace id", ws_a),
        ("host name", name),
        ("hostname", hostname),
        ("machine_token", machine_token),
    ] {
        assert!(
            !out.raw.contains(needle) && !json_contains_str(&out.body, needle),
            "{what} must not contain A's {label} ({needle}): {}",
            out.raw
        );
    }
    assert!(
        !out.raw.contains("flp_") && !json_contains_str(&out.body, "flp_"),
        "{what} must not contain A's pair_token (flp_): {}",
        out.raw
    );
    assert!(
        out.body.get("pair_token").is_none()
            || out.body["pair_token"].is_null()
            || out.body["pair_token"].as_str().is_none_or(str::is_empty),
        "{what} must not include pair_token: {}",
        out.body
    );
    assert!(
        out.body.get("machine_token").is_none()
            || out.body["machine_token"].is_null()
            || out.body["machine_token"].as_str().is_none_or(str::is_empty),
        "{what} must not include machine_token: {}",
        out.body
    );
}

/// catalog:list-host-foreign-id-404-omits-workspace — GET/PATCH/DELETE of
/// another workspace's host id on the caller's own path is 404 and must
/// not name that workspace. B on A's path stays 403 not-a-member
/// (list-worker-hosts-list-other-ws-403 / list-worker-hosts-mutate-other-ws-403).
/// Cluster hosts are list-cluster-host-label-undo /
/// list-patch-labels-cluster-not-cross-ws — this lock is a non-cluster pair.
#[tokio::test]
async fn host_foreign_id_get_patch_delete_is_404_and_omits_workspace() {
    let app = app().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    assert!(
        a_machine.starts_with("flm_"),
        "pair must mint a real machine token: {a_machine}"
    );
    let a_path = format!("{}/{a_hid}", hosts_path(&ws_a));

    let snap = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /worker-hosts/{{hid}} snapshot: {}",
        snap.body
    );
    assert_eq!(snap.body["id"].as_str(), Some(a_hid.to_string().as_str()));
    assert_eq!(
        snap.body["name"].as_str(),
        Some(a_name.as_str()),
        "snapshot name: {}",
        snap.body
    );
    assert_eq!(
        snap.body["workspace_id"].as_str(),
        Some(ws_a.as_str()),
        "snapshot workspace_id: {}",
        snap.body
    );
    let a_hostname = snap.body["hostname"]
        .as_str()
        .unwrap_or_else(|| panic!("A GET hostname after pair: {}", snap.body))
        .to_string();
    let snap_status = snap.body["status"].clone();
    assert!(
        !labels_contain(&snap.body, "cluster"),
        "this lock is a non-cluster paired host: {}",
        snap.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");
    let b_path = format!("{}/{a_hid}", hosts_path(&ws_b));
    let b_rename = format!("stolen-by-b-{}", Uuid::new_v4());
    assert_ne!(b_rename, a_name);

    for method in ["GET", "PATCH", "DELETE"] {
        let body = (method == "PATCH").then(|| json!({ "name": b_rename }));
        let out = call(&app, method, &b_path, Some(&jar_b), body).await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "B {method} of A's hid on B's path must not return A's row: {}",
            out.raw
        );
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "B {method} of A's hid on B's path must be 404: {}",
            out.raw
        );
        assert_oneshot_raw_omits_a_host_tenant(
            &out,
            &format!("B {method} 404 on B's path"),
            &ws_a,
            &a_name,
            &a_hostname,
            &a_machine,
        );
    }

    let after_b_path = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        after_b_path.status,
        StatusCode::OK,
        "A GET as A after B GET/PATCH/DELETE on B's path must still 200 (not revoked): {}",
        after_b_path.body
    );
    assert_eq!(
        after_b_path.body["id"].as_str(),
        Some(a_hid.to_string().as_str())
    );
    assert_eq!(
        after_b_path.body["name"].as_str(),
        Some(a_name.as_str()),
        "B PATCH on B's path must not rename A: {}",
        after_b_path.body
    );
    assert_ne!(
        after_b_path.body["name"].as_str(),
        Some(b_rename.as_str()),
        "B PATCH must not persist the stolen name: {}",
        after_b_path.body
    );
    assert_ne!(
        after_b_path.body["status"].as_str(),
        Some("disabled"),
        "B DELETE on B's path must not revoke A: {}",
        after_b_path.body
    );
    assert_eq!(
        after_b_path.body["status"], snap_status,
        "A status must match the snapshot after B's 404s: {}",
        after_b_path.body
    );

    for method in ["GET", "PATCH", "DELETE"] {
        let body = (method == "PATCH").then(|| json!({ "name": b_rename }));
        let out = call(&app, method, &a_path, Some(&jar_b), body).await;
        assert_eq!(
            out.status,
            StatusCode::FORBIDDEN,
            "B {method} on A's path must be 403 not-a-member: {}",
            out.raw
        );
        let err = out.body["error"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            err.contains("not a member"),
            "B {method} on A's path should say not a member, got {}: {}",
            err,
            out.raw
        );
        assert_oneshot_raw_omits_a_host_tenant(
            &out,
            &format!("B {method} 403 on A's path"),
            &ws_a,
            &a_name,
            &a_hostname,
            &a_machine,
        );
    }

    let after_a_path = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        after_a_path.status,
        StatusCode::OK,
        "A GET as A after B 403s must still 200: {}",
        after_a_path.body
    );
    assert_eq!(
        after_a_path.body["name"].as_str(),
        Some(a_name.as_str()),
        "B PATCH on A's path must not rename A: {}",
        after_a_path.body
    );

    let b_own_name = format!("b-own-{}", Uuid::new_v4());
    let b_created = call(
        &app,
        "POST",
        &hosts_path(&ws_b),
        Some(&jar_b),
        Some(json!({ "name": b_own_name })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::OK,
        "control: B POST /worker-hosts on B must still 200: {}",
        b_created.body
    );
    let b_hid = b_created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control create must return host.id: {}", b_created.body))
        .to_string();
    assert_ne!(
        b_hid,
        a_hid.to_string(),
        "control host must be a different row"
    );
    let b_got = call(
        &app,
        "GET",
        &format!("{}/{b_hid}", hosts_path(&ws_b)),
        Some(&jar_b),
        None,
    )
    .await;
    assert_eq!(
        b_got.status,
        StatusCode::OK,
        "control: B GET of B's hid must 200 so this is not a tautology that every host GET is 404: {}",
        b_got.body
    );
    assert_eq!(b_got.body["id"].as_str(), Some(b_hid.as_str()));
    assert_eq!(b_got.body["name"].as_str(), Some(b_own_name.as_str()));

    let still_a = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        still_a.status,
        StatusCode::OK,
        "A GET as A after control must still 200: {}",
        still_a.body
    );
    assert_eq!(
        still_a.body["name"].as_str(),
        Some(a_name.as_str()),
        "A's GET must still match the snapshot name: {}",
        still_a.body
    );
}

/// catalog:list-patch-labels-cluster-not-cross-ws — PATCH labels=["cluster"]
/// on a regular paired box must not make it visible or assignable on another
/// workspace. Cluster hosts are bootstrap-only; labels are informational.
#[tokio::test]
async fn patch_labels_cluster_must_not_make_paired_host_visible_to_other_workspace() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-laptop-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    assert!(
        a_machine.starts_with("flm_"),
        "pair must mint a real machine token: {a_machine}"
    );
    let a_path = format!("{}/{a_hid}", hosts_path(&ws_a));

    let snap = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET before PATCH: {}",
        snap.body
    );
    assert_eq!(snap.body["id"].as_str(), Some(a_hid.to_string().as_str()));
    assert_eq!(
        snap.body["name"].as_str(),
        Some(a_name.as_str()),
        "{}",
        snap.body
    );
    let a_hostname = snap.body["hostname"]
        .as_str()
        .unwrap_or_else(|| panic!("A GET hostname: {}", snap.body))
        .to_string();
    assert!(
        !labels_contain(&snap.body, "cluster"),
        "paired laptop must not start as cluster: {}",
        snap.body
    );

    // Control: A's own enqueue is claimable so later claim:null is not a
    // tautology that /me/lease never returns work.
    let ws_a_id = Uuid::parse_str(&ws_a).expect("ws a");
    let own = enqueue_spawn_for_host(&state, ws_a_id, a_hid).await;
    let own_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &a_machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        own_lease.status,
        StatusCode::OK,
        "A own-work lease: {}",
        own_lease.body
    );
    assert_eq!(
        own_lease.body["claim"]["id"].as_str(),
        Some(own.to_string().as_str()),
        "A must still lease home-workspace spawn_session: {}",
        own_lease.body
    );
    let own_token = own_lease.body["claim"]["lease_token"]
        .as_str()
        .expect("lease_token")
        .to_string();
    let own_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{own}/complete"),
        &a_machine,
        Some(json!({ "lease_token": own_token, "ok": true })),
    )
    .await;
    assert_eq!(
        own_done.status,
        StatusCode::OK,
        "complete A's own work: {}",
        own_done.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    let b_name = format!("b-laptop-{}", Uuid::new_v4());
    let (_b_hid, b_machine) =
        create_pair_and_heartbeat(&app, &jar_b, &ws_b, &b_name, true, false).await;

    let cluster = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["cluster"] })),
    )
    .await;
    assert_ne!(
        cluster.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PATCH labels=[cluster] must not 500: {}",
        cluster.body
    );
    assert!(
        cluster.status == StatusCode::BAD_REQUEST || cluster.status == StatusCode::OK,
        "PATCH labels=[cluster] must be 400 (bootstrap-only / informational) or 200: {} {}",
        cluster.status,
        cluster.body
    );

    assert_patch_cluster_not_cross_ws(
        &app,
        &state,
        &jar_a,
        &ws_a,
        &jar_b,
        &ws_b,
        a_hid,
        &a_name,
        &a_hostname,
        &a_machine,
        &b_machine,
        "after PATCH labels=[cluster]",
    )
    .await;

    let gpu = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["gpu"] })),
    )
    .await;
    assert_ne!(
        gpu.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PATCH labels=[gpu] must not 500: {}",
        gpu.body
    );
    if gpu.status == StatusCode::OK {
        assert!(
            labels_contain(&gpu.body, "gpu"),
            "PATCH labels=[gpu] may persist: {}",
            gpu.body
        );
        let again = call(&app, "GET", &a_path, Some(&jar_a), None).await;
        assert_eq!(
            again.status,
            StatusCode::OK,
            "A GET after gpu PATCH: {}",
            again.body
        );
        assert!(
            labels_contain(&again.body, "gpu"),
            "gpu label must persist on GET: {}",
            again.body
        );
    }
    assert_b_cannot_see_a_host(
        &app,
        &jar_b,
        &ws_b,
        a_hid,
        &a_name,
        &a_hostname,
        &a_machine,
        "after PATCH labels=[gpu]",
    )
    .await;

    let both = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["cluster", "gpu"] })),
    )
    .await;
    assert_ne!(
        both.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PATCH labels=[cluster, gpu] must not 500: {}",
        both.body
    );
    assert!(
        both.status == StatusCode::BAD_REQUEST || both.status == StatusCode::OK,
        "PATCH labels=[cluster, gpu] must be 400 or 200: {} {}",
        both.status,
        both.body
    );
    assert_patch_cluster_not_cross_ws(
        &app,
        &state,
        &jar_a,
        &ws_a,
        &jar_b,
        &ws_b,
        a_hid,
        &a_name,
        &a_hostname,
        &a_machine,
        &b_machine,
        "after PATCH labels=[cluster, gpu]",
    )
    .await;

    let boot_name = format!("cluster-boot-{}", Uuid::new_v4());
    let boot_secret = std::env::var("FLEET_BOOTSTRAP_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| TEST_BOOTSTRAP_SECRET.to_string());
    let boot = {
        let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(&boot_secret)).await;
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            &boot_secret,
            Some(json!({
                "hostname": boot_name,
                "platform": "linux-x86_64",
                "advertised_host": false,
                "advertised_container": true,
                "machine_id": Uuid::new_v4(),
                "workspace_id": ws_a
            })),
        )
        .await
    };
    assert_eq!(boot.status, StatusCode::OK, "bootstrap: {}", boot.body);
    assert!(
        boot.body["machine_token"]
            .as_str()
            .is_some_and(|t| t.starts_with("flm_")),
        "bootstrap must still mint a machine token: {}",
        boot.body
    );
    let boot_labels = boot.body["cp_settings"]["labels"]
        .as_array()
        .unwrap_or_else(|| panic!("bootstrap cp_settings.labels: {}", boot.body));
    assert!(
        boot_labels.iter().any(|l| l.as_str() == Some("cluster")),
        "bootstrap must still create a cluster-labeled host: {}",
        boot.body
    );
    let boot_hid = boot.body["host_id"]
        .as_str()
        .unwrap_or_else(|| panic!("bootstrap host_id: {}", boot.body));
    assert_ne!(boot_hid, a_hid.to_string().as_str());

    let still = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "A GET as A after bootstrap must still 200: {}",
        still.body
    );
    assert_eq!(still.body["id"].as_str(), Some(a_hid.to_string().as_str()));
    assert_eq!(still.body["name"].as_str(), Some(a_name.as_str()));
}

fn labels_contain(body: &Value, label: &str) -> bool {
    body["labels"]
        .as_array()
        .map(|a| a.iter().any(|v| v.as_str() == Some(label)))
        .unwrap_or(false)
}

fn assert_body_omits_a_host(
    out: &HttpOut,
    hid: &str,
    name: &str,
    hostname: &str,
    machine_token: &str,
    what: &str,
) {
    for (label, needle) in [
        ("hid", hid),
        ("name", name),
        ("hostname", hostname),
        ("machine_token", machine_token),
    ] {
        assert!(
            !out.raw.contains(needle) && !json_contains_str(&out.body, needle),
            "{what} must not include A's {label} ({needle}): {}",
            out.body
        );
    }
}

async fn assert_b_cannot_see_a_host(
    app: &axum::Router,
    jar_b: &CookieJar,
    ws_b: &str,
    a_hid: Uuid,
    a_name: &str,
    a_hostname: &str,
    a_machine: &str,
    what: &str,
) {
    let hid = a_hid.to_string();
    let list = call(app, "GET", &hosts_path(ws_b), Some(jar_b), None).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "{what} B GET list: {}",
        list.body
    );
    let hosts = list.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("{what} B list hosts: {}", list.body));
    assert!(
        hosts.iter().all(|h| h["id"].as_str() != Some(hid.as_str())),
        "{what} B GET /worker-hosts must not 200 with A's card {hid}: {}",
        list.body
    );
    assert_body_omits_a_host(
        &list,
        &hid,
        a_name,
        a_hostname,
        a_machine,
        &format!("{what} B GET list"),
    );

    let got = call(
        app,
        "GET",
        &format!("{}/{hid}", hosts_path(ws_b)),
        Some(jar_b),
        None,
    )
    .await;
    assert!(
        got.status == StatusCode::FORBIDDEN || got.status == StatusCode::NOT_FOUND,
        "{what} B GET /worker-hosts/{{hid}} must be 403/404, not 200 with A's card: {} {}",
        got.status,
        got.body
    );
    assert_body_omits_a_host(
        &got,
        &hid,
        a_name,
        a_hostname,
        a_machine,
        &format!("{what} B GET hid"),
    );
}

async fn assert_patch_cluster_not_cross_ws(
    app: &axum::Router,
    state: &AppState,
    jar_a: &CookieJar,
    ws_a: &str,
    jar_b: &CookieJar,
    ws_b: &str,
    a_hid: Uuid,
    a_name: &str,
    a_hostname: &str,
    a_machine: &str,
    b_machine: &str,
    what: &str,
) {
    let a_still = call(
        app,
        "GET",
        &format!("{}/{a_hid}", hosts_path(ws_a)),
        Some(jar_a),
        None,
    )
    .await;
    assert_eq!(
        a_still.status,
        StatusCode::OK,
        "{what} A's GET as A must still 200: {}",
        a_still.body
    );
    assert_eq!(
        a_still.body["id"].as_str(),
        Some(a_hid.to_string().as_str())
    );

    assert_b_cannot_see_a_host(app, jar_b, ws_b, a_hid, a_name, a_hostname, a_machine, what).await;

    let ws_b_id = Uuid::parse_str(ws_b).expect("ws b");
    let claim_id = enqueue_spawn_for_host(state, ws_b_id, a_hid).await;
    let b_lease = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/lease",
        b_machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &b_lease,
        &format!("{what} B /me/lease must not claim A's box"),
    );
    assert_claim_unleased(state, claim_id, &format!("{what} B enqueue for A's hid")).await;

    let a_lease = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/lease",
        a_machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &a_lease,
        &format!("{what} A's flm_ /me/lease after a B-only enqueue"),
    );
    assert_claim_unleased(
        state,
        claim_id,
        &format!("{what} B-only spawn after A's lease poll"),
    )
    .await;
}

async fn assert_host_name(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    want: &str,
    settings_gen: Option<i64>,
    what: &str,
) {
    let got = call(app, "GET", path, Some(jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{what} GET: {}", got.body);
    assert_eq!(
        got.body["name"].as_str(),
        Some(want),
        "{what} must not persist a different name: {}",
        got.body
    );
    if let Some(want_gen) = settings_gen {
        assert_eq!(
            got.body["settings_gen"].as_i64().expect("settings_gen"),
            want_gen,
            "{what} must not bump settings_gen: {}",
            got.body
        );
    }
}

/// catalog:unit-patch-name — PATCH name persists; empty/too-long 400;
/// other-ws 403; plane rename must not invent a `worker_hosts` row.
#[tokio::test]
async fn rename_host_patch_persists_rejects_empty_too_long() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let seed = format!("rename-seed-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": seed })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(created.body["host"]["name"], seed);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let isolation = created.body["host"]["isolation"].clone();
    let gen0 = created.body["host"]["settings_gen"]
        .as_i64()
        .expect("settings_gen");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let (_other_email, other_jar, _other_ws) = signup_and_login(&app).await;
    let foreign_member = call(
        &app,
        "PATCH",
        &host_path,
        Some(&other_jar),
        Some(json!({ "name": "foreign" })),
    )
    .await;
    assert_eq!(
        foreign_member.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on this workspace PATCH name must be 403: {}",
        foreign_member.body
    );
    let foreign_err = foreign_member.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        foreign_err.contains("member") || foreign_err.contains("owner"),
        "other-ws 403 should say not a member / owner required: {}",
        foreign_member.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        Some(gen0),
        "after other-ws 403",
    )
    .await;

    for empty in ["", "   ", "\t\n"] {
        let out = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "name": empty })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "empty name {empty:?} must not 500: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "empty/whitespace PATCH name must be 400, not a no-op 200: {empty:?} {}",
            out.body
        );
        assert_eq!(
            out.body["error"].as_str(),
            Some("name is required"),
            "empty PATCH name error: {}",
            out.body
        );
        assert!(
            out.body.get("name").is_none() || out.body["name"] == seed,
            "400 body must not advertise a wiped name: {}",
            out.body
        );
    }
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        Some(gen0),
        "after empty reject",
    )
    .await;

    for n in [256_usize, 10_000] {
        let too_long = "x".repeat(n);
        let out = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "name": too_long })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "name len {n} must not 500: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "name longer than 255 must be 400: len={n} {}",
            out.body
        );
        assert_eq!(
            out.body["error"].as_str(),
            Some("name is too long"),
            "too-long PATCH name error: {}",
            out.body
        );
        assert_ne!(
            out.body["name"].as_str(),
            Some(too_long.as_str()),
            "400 must not echo/persist the huge name: {}",
            out.body
        );
    }
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        Some(gen0),
        "after too-long reject",
    )
    .await;

    let saved = format!("rename-saved-{}", Uuid::new_v4());
    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "name": format!("  {saved}  ") })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "valid rename: {}",
        patched.body
    );
    assert_eq!(
        patched.body["name"], saved,
        "PATCH name trims: {}",
        patched.body
    );
    assert_eq!(
        patched.body["isolation"], isolation,
        "rename must not change isolation: {}",
        patched.body
    );
    let gen1 = patched.body["settings_gen"]
        .as_i64()
        .expect("patched settings_gen");
    assert_eq!(
        gen1,
        gen0 + 1,
        "name is a CP setting so rename bumps gen: {}",
        patched.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &saved,
        Some(gen1),
        "GET after persist",
    )
    .await;

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    let hosts = list.body["hosts"].as_array().expect("hosts array");
    let listed = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("list missing renamed host: {}", list.body));
    assert_eq!(listed["name"], saved, "list name: {}", list.body);

    let at_max = "n".repeat(255);
    let max_ok = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "name": at_max })),
    )
    .await;
    assert_eq!(
        max_ok.status,
        StatusCode::OK,
        "exactly 255 bytes must be accepted (not a tautology that every PATCH 400s): {}",
        max_ok.body
    );
    assert_eq!(max_ok.body["name"], at_max, "{}", max_ok.body);
    assert_eq!(
        max_ok.body["name"].as_str().map(str::len),
        Some(255),
        "stored name must be 255 bytes: {}",
        max_ok.body
    );
}

#[tokio::test]
async fn list_has_no_synthetic_control_plane() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws uuid");
    let plane_id = fleet_db::control_plane_runner_id(ws_id);

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    let hosts = list.body["hosts"].as_array().expect("hosts");
    assert!(
        hosts.iter().all(|h| h["kind"] != "control_plane"),
        "API process is not a runner: {}",
        list.body
    );
    assert!(
        hosts
            .iter()
            .all(|h| h["workspace_id"].as_str() != Some(ws.as_str())),
        "fresh workspace has no worker_hosts of its own: {}",
        list.body
    );

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{plane_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "allow_host": false, "allow_container": false })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::NOT_FOUND,
        "patch: {}",
        patched.body
    );

    let denied = call(
        &app,
        "DELETE",
        &format!("{}/{plane_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND, "{}", denied.body);
}

#[tokio::test]
async fn control_plane_does_not_pair_or_appear_assignable() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let plane_id = fleet_db::control_plane_runner_id(ws_id);

    let pair = call(
        &app,
        "POST",
        &format!("{}/{plane_id}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(pair.status, StatusCode::NOT_FOUND, "{}", pair.body);

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !listed.contains(&plane_id),
        "plane must not be in list_assignable: {listed:?}"
    );
}

fn json_has_key(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(map) => {
            map.contains_key(key) || map.values().any(|child| json_has_key(child, key))
        }
        Value::Array(items) => items.iter().any(|child| json_has_key(child, key)),
        _ => false,
    }
}

fn json_contains_str(v: &Value, needle: &str) -> bool {
    match v {
        Value::String(s) => s.contains(needle),
        Value::Array(items) => items.iter().any(|child| json_contains_str(child, needle)),
        Value::Object(map) => map.values().any(|child| json_contains_str(child, needle)),
        _ => false,
    }
}

fn assert_host_object_omits_pair_secrets(host: &Value, pair_token: &str) {
    for key in ["pair_token", "pair_token_hash"] {
        assert!(!json_has_key(host, key), "host object leaked {key}: {host}");
    }
    assert!(
        !json_contains_str(host, pair_token),
        "host object leaked pair token plaintext: {host}"
    );
}

/// Create / rotate envelope: plaintext once at top-level `pair_token` +
/// `install_hint`. Host object and every other field must omit it (and
/// `pair_token_hash`). `old_token` is the previous one-shot after rotate.
fn assert_create_or_rotate_pair_once(out: &HttpOut, old_token: Option<&str>) -> String {
    assert_eq!(out.status, StatusCode::OK, "create/rotate: {}", out.body);
    let pair_token = out.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "create/rotate must return top-level pair_token: {}",
                out.body
            )
        })
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );
    assert!(
        out.raw.contains(&pair_token),
        "HTTP body must include the one-shot pair_token"
    );
    let hint = out.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("create/rotate must return install_hint: {}", out.body));
    assert!(
        hint.contains(&pair_token),
        "install_hint must contain the one-shot token: {hint}"
    );
    assert!(
        !json_has_key(&out.body, "pair_token_hash"),
        "create/rotate must not return pair_token_hash: {}",
        out.body
    );
    let host = out
        .body
        .get("host")
        .unwrap_or_else(|| panic!("create/rotate must return host object: {}", out.body));
    assert_host_object_omits_pair_secrets(host, &pair_token);
    assert_plaintext_only_at_create_fields(&out.body, &pair_token);
    if let Some(old) = old_token {
        assert_ne!(pair_token, old, "rotate must issue a different token");
        assert!(
            !hint.contains(old),
            "rotate install_hint must not repeat the old token: {hint}"
        );
        assert_host_object_omits_pair_secrets(host, old);
        assert!(
            !out.raw.contains(old),
            "rotate HTTP body leaked the old pair token"
        );
    }
    pair_token
}

fn assert_plaintext_only_at_create_fields(v: &Value, pair_token: &str) {
    fn walk(v: &Value, path: &[String], token: &str) {
        match v {
            Value::String(s) => {
                if s.contains(token) {
                    let allowed = path == ["pair_token"] || path == ["install_hint"];
                    assert!(
                        allowed,
                        "pair token plaintext at {path:?} (only top-level pair_token and install_hint): {s}"
                    );
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let mut next = path.to_vec();
                    next.push(i.to_string());
                    walk(child, &next, token);
                }
            }
            Value::Object(map) => {
                for (k, child) in map {
                    let mut next = path.to_vec();
                    next.push(k.clone());
                    walk(child, &next, token);
                }
            }
            _ => {}
        }
    }
    walk(v, &[], pair_token);
}

fn assert_later_read_omits_pair_plaintext(out: &HttpOut, tokens: &[&str]) {
    assert_eq!(out.status, StatusCode::OK, "later GET/list: {}", out.body);
    for key in [
        "pair_token",
        "pair_token_hash",
        "install_hint",
        "machine_token",
        "machine_token_hash",
        "token_hash",
    ] {
        assert!(
            !json_has_key(&out.body, key),
            "later GET/list leaked {key}: {}",
            out.body
        );
    }
    for token in tokens {
        assert!(
            !out.raw.contains(token),
            "later GET/list raw body leaked pair token plaintext"
        );
        assert!(
            !json_contains_str(&out.body, token),
            "later GET/list leaked pair token plaintext: {}",
            out.body
        );
        assert!(
            !out.raw.contains("install_hint"),
            "later GET/list must not include install_hint: {}",
            out.raw
        );
    }
}

/// Later GET/list/heartbeat/lease: snapshot `flm_…` from pair/bootstrap must
/// not appear in the raw HTTP body (host object, envelope extras,
/// install_hint). `machine_token` / `machine_token_hash` keys stay absent.
fn assert_later_http_omits_machine_plaintext(out: &HttpOut, tokens: &[&str], what: &str) {
    assert_eq!(out.status, StatusCode::OK, "{what}: {}", out.body);
    for key in ["machine_token", "machine_token_hash"] {
        assert!(
            !json_has_key(&out.body, key),
            "{what} leaked {key}: {}",
            out.body
        );
    }
    for token in tokens {
        assert!(
            !token.is_empty() && token.starts_with("flm_"),
            "{what}: snapshot must be a real flm_ machine token, got {token:?}"
        );
        assert!(
            !out.raw.contains(token),
            "{what} raw body leaked machine token plaintext: {}",
            out.raw
        );
        assert!(
            !json_contains_str(&out.body, token),
            "{what} leaked machine token plaintext: {}",
            out.body
        );
    }
}

/// GET/PATCH/DELETE on the synthetic plane id must never insert a hosts row.
async fn assert_not_a_hosts_row(state: &AppState, workspace_id: Uuid, plane_id: Uuid) {
    let row = fleet_db::repos::worker_hosts::get(&state.db, plane_id)
        .await
        .expect("worker_hosts get");
    assert!(
        row.is_none(),
        "control-plane id must not exist as a worker_hosts row: {row:?}"
    );
    let listed = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace");
    assert!(
        listed.iter().all(|h| h.id != plane_id),
        "list_for_workspace must not include plane id {plane_id}: {listed:?}"
    );
    let assignable = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(30),
    )
    .await
    .expect("list_assignable");
    assert!(
        assignable.iter().all(|h| h.id != plane_id),
        "plane must not be assignable: {assignable:?}"
    );
}
#[tokio::test]
async fn isolation_defaults_native_and_patch_sandboxed() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;

    let got = call(
        &app,
        "GET",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
    assert_eq!(got.body["isolation"], "native", "{}", got.body);

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        listed,
        vec![host_id],
        "native host is assignable: {listed:?}"
    );

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "isolation": "sandboxed" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "patch: {}", patched.body);
    assert_eq!(
        patched.body["isolation"], "sandboxed",
        "PATCH may set the placeholder: {}",
        patched.body
    );

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert!(
        listed.is_empty(),
        "sandboxed must not be assignable: {listed:?}"
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert!(
        lease.body["claim"].is_null(),
        "sandboxed host must not lease: {}",
        lease.body
    );

    let bad = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "isolation": "k8s" })),
    )
    .await;
    assert_eq!(
        bad.status,
        StatusCode::BAD_REQUEST,
        "bad isolation: {}",
        bad.body
    );
    assert!(
        bad.body["error"]
            .as_str()
            .unwrap_or("")
            .contains("isolation"),
        "error should name isolation: {}",
        bad.body
    );

    let back = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "isolation": "native" })),
    )
    .await;
    assert_eq!(back.status, StatusCode::OK, "restore: {}", back.body);
    assert_eq!(back.body["isolation"], "native");
    let listed = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        listed,
        vec![host_id],
        "native again is assignable: {listed:?}"
    );
}

/// POST `isolation=sandboxed` is 400 (`create is native-only; isolation must
/// be native`) and must not insert a worker_hosts row. A 200 that silently
/// creates a native assignable host is a fail. PATCH to sandboxed stays
/// allowed (`isolation_defaults_native_and_patch_sandboxed`). Omitted
/// isolation still inserts native.
#[tokio::test]
async fn post_isolation_sandboxed_is_400_and_does_not_insert() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let body = json!({
        "name": format!("iso-sandboxed-{}", Uuid::new_v4()),
        "isolation": "sandboxed"
    });
    let out = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "POST isolation=sandboxed must be 400: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("isolation must be native"),
        "POST isolation=sandboxed must reject isolation: {}",
        out.body
    );
    assert_post_cloud_kind_rejected(&app, &state, &jar, &ws, body).await;
}

/// Catalog: HTTP must not create cloud hosts. POST `kind=cloud` / `kind=aws`
/// (and `isolation=cloud` on create) is 400 + no `worker_hosts` row.
/// A 200 that silently creates a native assignable host is a fail.
/// Control: POST `{"name":"ok"}` without kind is still 200 native and, after
/// pair+heartbeat, *is* in `list_assignable` so this is not a tautology.
#[tokio::test]
async fn post_kind_cloud_and_aws_http_does_not_create_assignable_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");

    let before = workspace_host_ids(&state, ws_id).await;
    assert!(
        before.is_empty(),
        "fresh workspace has no worker_hosts rows: {before:?}"
    );

    for body in [
        json!({ "name": "cloud-box", "kind": "cloud" }),
        json!({ "name": "aws-box", "kind": "aws" }),
        json!({ "name": "iso-cloud", "isolation": "cloud" }),
    ] {
        assert_post_cloud_kind_rejected(&app, &state, &jar, &ws, body).await;
    }

    let after_rejects = workspace_host_ids(&state, ws_id).await;
    assert!(
        after_rejects.is_empty(),
        "cloud/aws POST must not insert a worker_hosts row: {after_rejects:?}"
    );
    let listed = worker_hosts_assignable(&state, &ws).await;
    // list_assignable also returns cluster-label hosts from other workspaces
    // (30s heartbeat window). Those are not rows from POST kind=cloud/aws.
    assert!(
        listed.iter().all(|id| !after_rejects.contains(id)),
        "rejected cloud POST must not appear in list_assignable: {listed:?}"
    );

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    let hosts = list.body["hosts"].as_array().expect("hosts array");
    // GET/list embeds the synthetic control plane and, for 30 minutes,
    // cluster-label hosts from other workspaces. Those are not created by
    // POST kind=cloud/aws. This workspace must not have a kind=host row.
    let own_kind_host: Vec<&Value> = hosts
        .iter()
        .filter(|h| h["kind"] == "host" && h["workspace_id"].as_str() == Some(ws.as_str()))
        .collect();
    assert!(
        own_kind_host.is_empty(),
        "GET/list must not present a created kind=host after rejected cloud POST: {}",
        list.body
    );
    for h in hosts {
        let Some(id) = h["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
            continue;
        };
        if h["workspace_id"].as_str() != Some(ws.as_str()) {
            continue;
        }
        assert!(
            !(h["kind"] == "host" && listed.contains(&id)),
            "GET/list must not present a created kind=host as assignable after rejected cloud POST: {h}"
        );
    }

    // Control: omitted kind still creates a native host that *is* assignable.
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "ok" })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "POST without kind must still 200 native: {}",
        created.body
    );
    assert_eq!(
        created.body["host"]["isolation"], "native",
        "control create must stay native: {}",
        created.body
    );
    assert_eq!(
        created.body["host"]["kind"], "host",
        "control create is a self-hosted host: {}",
        created.body
    );
    let ok_id = Uuid::parse_str(
        created.body["host"]["id"]
            .as_str()
            .expect("control host.id"),
    )
    .expect("control host uuid");
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("control pair_token")
        .to_string();
    assert!(
        !worker_hosts_assignable(&state, &ws).await.contains(&ok_id),
        "pending unpaired host is not yet assignable"
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "ok",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "control pair: {}",
        paired.body
    );
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token");
    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "control heartbeat: {}", hb.body);

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert!(
        listed.contains(&ok_id),
        "control native host after pair+heartbeat must be in list_assignable: {listed:?}"
    );
    let own = workspace_host_ids(&state, ws_id).await;
    assert_eq!(
        own,
        vec![ok_id],
        "control is the only workspace-owned worker_hosts row: {own:?}"
    );
    let own_assignable: Vec<Uuid> = listed
        .iter()
        .copied()
        .filter(|id| own.contains(id))
        .collect();
    assert_eq!(
        own_assignable,
        vec![ok_id],
        "control native host after pair+heartbeat must be the workspace's assignable id: {listed:?}"
    );

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "list after control: {}",
        list.body
    );
    let hosts = list.body["hosts"].as_array().expect("hosts");
    let ok = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(ok_id.to_string().as_str()))
        .unwrap_or_else(|| panic!("list missing paired native: {}", list.body));
    assert_eq!(ok["kind"], "host", "control is kind=host: {ok}");
    assert_eq!(ok["isolation"], "native", "control stays native: {ok}");
}

/// PATCH isolation=cloud / isolation=aws is 400 (same as k8s). Existing
/// native host stays native and, if already paired, remains the only
/// assignable id — do not persist cloud as native.
#[tokio::test]
async fn post_kind_cloud_patch_isolation_rejected_native_stays_assignable() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        listed,
        vec![host_id],
        "paired native is the only assignable id: {listed:?}"
    );

    for isolation in ["cloud", "aws"] {
        let patched = call(
            &app,
            "PATCH",
            &format!("{}/{host_id}", hosts_path(&ws)),
            Some(&jar),
            Some(json!({ "isolation": isolation })),
        )
        .await;
        assert_eq!(
            patched.status,
            StatusCode::BAD_REQUEST,
            "PATCH isolation={isolation} must be 400 (same as k8s): {}",
            patched.body
        );
        assert!(
            patched.body["error"]
                .as_str()
                .unwrap_or("")
                .contains("isolation"),
            "error should name isolation: {}",
            patched.body
        );

        let row = fleet_db::repos::worker_hosts::get(&state.db, host_id)
            .await
            .expect("get")
            .expect("host row");
        assert_eq!(
            row.isolation, "native",
            "PATCH isolation={isolation} must not persist (GET remaps unknown → native): stored={}",
            row.isolation
        );

        let got = call(
            &app,
            "GET",
            &format!("{}/{host_id}", hosts_path(&ws)),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(got.status, StatusCode::OK, "get: {}", got.body);
        assert_eq!(
            got.body["isolation"], "native",
            "GET must still show native after rejected PATCH {isolation}: {}",
            got.body
        );
        assert_eq!(got.body["kind"], "host", "{}", got.body);

        let listed = worker_hosts_assignable(&state, &ws).await;
        assert_eq!(
            listed,
            vec![host_id],
            "after PATCH isolation={isolation} the paired native is still the only assignable id: {listed:?}"
        );
    }
}

async fn workspace_host_ids(state: &AppState, workspace_id: Uuid) -> Vec<Uuid> {
    fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace")
        .into_iter()
        .map(|h| h.id)
        .collect()
}

/// 400 + no new row. If the handler ignored kind and 200'd a host, pair +
/// heartbeat so `list_assignable` is a real check (pending rows are not
/// assignable anyway).
async fn assert_post_cloud_kind_rejected(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
    body: Value,
) {
    let ws_id = Uuid::parse_str(ws).expect("ws");
    let before = workspace_host_ids(state, ws_id).await;

    let out = call(app, "POST", &hosts_path(ws), Some(jar), Some(body.clone())).await;

    let leaked_id = out.body["host"]["id"]
        .as_str()
        .or_else(|| out.body["id"].as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    if let (Some(_id), Some(token)) = (leaked_id, out.body["pair_token"].as_str()) {
        let paired = call(
            app,
            "POST",
            "/api/worker-hosts/pair",
            None,
            Some(json!({
                "token": token,
                "hostname": "leaked-cloud",
                "platform": "linux-x86_64",
                "advertised_host": true,
                "advertised_container": false,
            })),
        )
        .await;
        if paired.status == StatusCode::OK {
            if let Some(machine) = paired.body["machine_token"].as_str() {
                let _ = call_bearer(
                    app,
                    "POST",
                    "/api/worker-hosts/me/heartbeat",
                    machine,
                    Some(json!({
                        "advertised_host": true,
                        "advertised_container": false,
                        "current_jobs": 0,
                        "max_concurrent": 2
                    })),
                )
                .await;
            }
        }
    }

    let after = workspace_host_ids(state, ws_id).await;
    let new_ids: Vec<Uuid> = after
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    let assignable = worker_hosts_assignable(state, ws).await;

    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "POST {body} must be 400 + no worker_hosts row; a 200 that silently \
         creates a native assignable host is a fail: {}",
        out.body
    );
    assert!(
        new_ids.is_empty(),
        "POST {body} must not insert a worker_hosts row: new={new_ids:?} body={}",
        out.body
    );
    for id in new_ids.iter().chain(leaked_id.iter()) {
        assert!(
            !assignable.contains(id),
            "POST {body} row {id} must be absent from list_assignable even after \
             pair+heartbeat: {assignable:?}"
        );
    }

    let list = call(app, "GET", &hosts_path(ws), Some(jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    if let Some(hosts) = list.body["hosts"].as_array() {
        for h in hosts {
            let Some(id) = h["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                continue;
            };
            // Foreign cluster-label hosts appear on every workspace list
            // (30m window). Only a row this POST created is in scope.
            if !new_ids.contains(&id) && leaked_id != Some(id) {
                continue;
            }
            assert!(
                !(h["kind"] == "host" && assignable.contains(&id)),
                "GET/list must not present POST {body} as kind=host assignable: {h}"
            );
        }
    }
}

async fn worker_hosts_assignable(state: &AppState, ws: &str) -> Vec<Uuid> {
    let workspace_id = Uuid::parse_str(ws).expect("ws");
    fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(30),
    )
    .await
    .expect("list_assignable")
    .into_iter()
    .filter(|h| h.workspace_id == workspace_id)
    .map(|h| h.id)
    .collect()
}

fn assert_pair_expires_in_one_hour(
    pair_expires_at: &Value,
    issued_from: chrono::DateTime<chrono::Utc>,
    issued_to: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    let raw = pair_expires_at
        .as_str()
        .unwrap_or_else(|| panic!("pair_expires_at must be an RFC3339 string: {pair_expires_at}"));
    let expires = chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("pair_expires_at must be RFC3339 ({e}): {raw}"))
        .with_timezone(&chrono::Utc);
    let lo = issued_from + chrono::Duration::hours(1);
    let hi = issued_to + chrono::Duration::hours(1);
    assert!(
        expires >= lo && expires <= hi,
        "catalog pair token TTL is 1 hour; pair_expires_at={expires} expected {lo}..={hi}"
    );
    expires
}

fn pair_body(token: &str, hostname: &str) -> Value {
    json!({
        "token": token,
        "hostname": hostname,
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false,
    })
}

/// Catalog: pair token expires in one hour and cannot be reused.
/// HTTP create/rotate must issue `pair_expires_at = now + 1h` (PAIR_TTL);
/// guest `POST /api/worker-hosts/pair` fails for a reused or expired token.
#[tokio::test]
async fn pair_token_expires_in_one_hour_and_cannot_be_reused() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let issued_from = chrono::Utc::now();
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "ttl-box" })),
    )
    .await;
    let issued_to = chrono::Utc::now();
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let hid_uuid = Uuid::parse_str(&hid).expect("host id uuid");
    let http_expires =
        assert_pair_expires_in_one_hour(&created.body["pair_expires_at"], issued_from, issued_to);

    let stored_expires: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        r#"
        SELECT expires_at
        FROM worker_host_pair_tokens
        WHERE host_id = $1 AND used_at IS NULL
        "#,
    )
    .bind(hid_uuid)
    .fetch_one(&state.db)
    .await
    .expect("persisted pair token expires_at");
    let persist_skew = (stored_expires - http_expires).num_milliseconds().abs();
    assert!(
        persist_skew <= 2_000,
        "DB expires_at must match HTTP pair_expires_at: http={http_expires} db={stored_expires}"
    );

    let rotate_from = chrono::Utc::now();
    let rotated = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    let rotate_to = chrono::Utc::now();
    assert_eq!(rotated.status, StatusCode::OK, "rotate: {}", rotated.body);
    let rotated_token = rotated.body["pair_token"]
        .as_str()
        .expect("rotated pair_token")
        .to_string();
    assert_ne!(rotated_token, pair_token, "rotate must issue a new token");
    assert_pair_expires_in_one_hour(&rotated.body["pair_expires_at"], rotate_from, rotate_to);

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&rotated_token, "ttl-box")),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "guest pair inside 1h window: {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token")
        .to_string();
    assert!(machine.starts_with("flm_"), "machine_token: {machine}");

    let reuse = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&rotated_token, "ttl-box")),
    )
    .await;
    assert_eq!(
        reuse.status,
        StatusCode::CONFLICT,
        "reused pair token must fail on the guest: {} {}",
        reuse.status,
        reuse.body
    );
    let reuse_err = reuse.body["error"].as_str().unwrap_or(&reuse.raw);
    assert!(
        reuse_err.contains("used"),
        "reused pair must say used: {reuse_err}"
    );

    // Fresh host: age the persisted 1h window, then guest pair must fail.
    let stale_from = chrono::Utc::now();
    let stale_created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "ttl-stale" })),
    )
    .await;
    let stale_to = chrono::Utc::now();
    assert_eq!(
        stale_created.status,
        StatusCode::OK,
        "stale create: {}",
        stale_created.body
    );
    let stale_token = stale_created.body["pair_token"]
        .as_str()
        .expect("stale pair_token")
        .to_string();
    let stale_hid = Uuid::parse_str(
        stale_created.body["host"]["id"]
            .as_str()
            .expect("stale host.id"),
    )
    .expect("stale host uuid");
    assert_pair_expires_in_one_hour(&stale_created.body["pair_expires_at"], stale_from, stale_to);
    sqlx::query(
        "UPDATE worker_host_pair_tokens SET expires_at = now() - interval '1 second' WHERE host_id = $1",
    )
    .bind(stale_hid)
    .execute(&state.db)
    .await
    .expect("elapse the 1h pair token");

    let expired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&stale_token, "ttl-stale")),
    )
    .await;
    assert_eq!(
        expired.status,
        StatusCode::CONFLICT,
        "expired pair token must fail on the guest: {} {}",
        expired.status,
        expired.body
    );
    let expired_err = expired.body["error"].as_str().unwrap_or(&expired.raw);
    assert!(
        expired_err.contains("expired"),
        "expired pair must say expired: {expired_err}"
    );
    let still_pending: String = sqlx::query_scalar("SELECT status FROM worker_hosts WHERE id = $1")
        .bind(stale_hid)
        .fetch_one(&state.db)
        .await
        .expect("stale host status");
    assert_eq!(
        still_pending, "pending",
        "expired guest pair must not promote the host"
    );
}

#[tokio::test]
async fn pair_once_second_pair_rejected() {
    // Unset WORKER_PUBLIC_BASE so this sibling keeps the API_PUBLIC_BASE fallback.
    let _env = PublicBaseEnvGuard::lock_and_set(None, None).await;
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "box" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let pair_body = json!({
        "token": pair_token,
        "hostname": "box",
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false,
    });
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body.clone()),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    assert_eq!(paired.body["workspace_id"].as_str(), Some(ws.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token")
        .to_string();
    assert!(machine.starts_with("flm_"), "machine_token: {machine}");
    assert_eq!(paired.body["control_plane_url"], "http://localhost:8080");
    assert!(paired.body["cp_settings"].is_object());

    let again = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body),
    )
    .await;
    assert!(
        again.status == StatusCode::UNAUTHORIZED || again.status == StatusCode::CONFLICT,
        "second pair should be 401 or 409, got {}: {}",
        again.status,
        again.body
    );

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let host = listed.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .expect("paired host in list");
    assert_eq!(
        host["machine_id"].as_str(),
        Some(hid.as_str()),
        "pair assigns durable machine_id: {}",
        listed.body
    );
}

/// Empty / whitespace hostname or platform must 400 before consume.
/// Burning the oneshot flp_ (or minting a nameless cluster row) is the hole.
#[tokio::test]
async fn pair_empty_hostname_or_platform_is_400_and_does_not_consume() {
    let (app, state) = app_with_state().await;
    let _secret = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("empty-hn-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );
    assert_host_still_pending(&app, &jar, &ws, &hid, "after create").await;

    let listed_before = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_before.status,
        StatusCode::OK,
        "GET /worker-hosts before empty pair/bootstrap: {}",
        listed_before.body
    );
    let before_hosts = listed_before.body["hosts"].as_array().expect("hosts array");
    let before_count = before_hosts.len();
    let before_ids: Vec<String> = before_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    let repo_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let repo_before_ids: Vec<Uuid> = repo_before.iter().map(|h| h.id).collect();

    let unique_hn = format!("ok-hn-{}", Uuid::new_v4());
    for (label, hostname, platform, want) in [
        ("empty hostname", "", "linux-x86_64", "hostname is required"),
        (
            "whitespace hostname",
            "   ",
            "linux-x86_64",
            "hostname is required",
        ),
        (
            "empty platform",
            unique_hn.as_str(),
            "",
            "platform is required",
        ),
        (
            "whitespace platform",
            unique_hn.as_str(),
            "   ",
            "platform is required",
        ),
    ] {
        let out = call(
            &app,
            "POST",
            "/api/worker-hosts/pair",
            None,
            Some(json!({
                "token": pair_token,
                "hostname": hostname,
                "platform": platform,
                "advertised_host": true,
                "advertised_container": false,
            })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "POST /pair {label} must not 200 pair: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "POST /pair {label} must not 401 expired (that would mean the flp_ was checked/consumed): {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::CONFLICT,
            "POST /pair {label} must not 409 used: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "POST /pair {label} must be 400 {want}: {}",
            out.body
        );
        let err = out.body["error"].as_str().unwrap_or(&out.raw);
        assert!(
            err.contains(want),
            "POST /pair {label} must say {want:?}, got {err:?}: {}",
            out.body
        );
        assert_pair_body_omits_machine(&out, &format!("POST /pair {label}"));
        assert!(
            !out.raw.contains("flm_") && !out.raw.contains("control_plane_url"),
            "POST /pair {label} must not leak machine_token / control_plane_url: {}",
            out.raw
        );
        assert_host_still_pending(&app, &jar, &ws, &hid, &format!("after POST /pair {label}"))
            .await;
    }

    for (label, hostname, platform, want) in [
        ("empty hostname", "", "linux-x86_64", "hostname is required"),
        (
            "whitespace hostname",
            "   ",
            "linux-x86_64",
            "hostname is required",
        ),
        (
            "empty platform",
            unique_hn.as_str(),
            "",
            "platform is required",
        ),
        (
            "whitespace platform",
            unique_hn.as_str(),
            "   ",
            "platform is required",
        ),
    ] {
        let out = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            TEST_BOOTSTRAP_SECRET,
            Some(json!({
                "hostname": hostname,
                "platform": platform,
                "advertised_host": true,
                "advertised_container": false,
                "workspace_id": ws,
            })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "POST /bootstrap {label} must not 200 pair: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::NOT_FOUND,
            "POST /bootstrap {label} must not 404 unset (TEST_BOOTSTRAP_SECRET is configured): {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "POST /bootstrap {label} must not 401 (secret is TEST_BOOTSTRAP_SECRET): {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "POST /bootstrap {label} must be 400 {want}: {}",
            out.body
        );
        let err = out.body["error"].as_str().unwrap_or(&out.raw);
        assert!(
            err.contains(want),
            "POST /bootstrap {label} must say {want:?}, got {err:?}: {}",
            out.body
        );
        assert!(
            out.body.get("machine_token").is_none()
                || out.body["machine_token"].is_null()
                || out.body["machine_token"].as_str().is_none_or(str::is_empty),
            "POST /bootstrap {label} must not include machine_token: {}",
            out.body
        );
        assert_pair_body_omits_machine(&out, &format!("POST /bootstrap {label}"));
        assert!(
            !out.raw.contains("flm_"),
            "POST /bootstrap {label} must not leak a machine token: {}",
            out.raw
        );
    }

    let listed_after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_after.status,
        StatusCode::OK,
        "GET /worker-hosts after empty pair/bootstrap: {}",
        listed_after.body
    );
    let after_hosts = listed_after.body["hosts"].as_array().expect("hosts array");
    let after_ids: Vec<String> = after_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        before_ids.iter().all(|id| after_ids.contains(id)),
        "empty pair/bootstrap must not drop a GET /worker-hosts row: {after_ids:?} vs {before_ids:?}"
    );
    assert_eq!(
        after_hosts
            .iter()
            .filter(|h| before_ids
                .iter()
                .any(|id| h["id"].as_str() == Some(id.as_str())))
            .count(),
        before_count,
        "empty pair/bootstrap must not replace this workspace's GET snapshot: {}",
        listed_after.body
    );
    assert!(
        after_hosts.iter().all(|h| {
            h["hostname"].as_str().is_none_or(|s| !s.trim().is_empty())
                && h["name"].as_str() != Some("")
                && h["hostname"].as_str() != Some(unique_hn.as_str())
        }),
        "empty pair/bootstrap must not list a nameless host or consume as {unique_hn}: {}",
        listed_after.body
    );

    let repo_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let repo_after_ids: Vec<Uuid> = repo_after.iter().map(|h| h.id).collect();
    assert_eq!(
        repo_after_ids, repo_before_ids,
        "empty bootstrap must not insert a worker_hosts row for this workspace"
    );
    assert!(
        repo_after.iter().all(|h| {
            h.hostname.as_deref().is_none_or(|s| !s.trim().is_empty())
                && !h.name.trim().is_empty()
                && h.hostname.as_deref() != Some(unique_hn.as_str())
        }),
        "empty bootstrap must not persist hostname \"\" on this workspace: {:?}",
        repo_after
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone()))
            .collect::<Vec<_>>()
    );
    let cluster_after = fleet_db::repos::worker_hosts::list_cluster(&state.db)
        .await
        .expect("list_cluster after");
    assert!(
        cluster_after.iter().all(|h| h.workspace_id != workspace_id),
        "empty bootstrap must not mint a cluster-host row in this workspace: {:?}",
        cluster_after
            .iter()
            .filter(|h| h.workspace_id == workspace_id)
            .map(|h| (h.id, h.name.clone(), h.hostname.clone()))
            .collect::<Vec<_>>()
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": unique_hn,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "control: unused flp_ + unique hostname + linux-x86_64 must still 200 (empty probes must not consume): {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    assert_eq!(paired.body["workspace_id"].as_str(), Some(ws.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("control pair must return machine_token")
        .to_string();
    assert!(
        machine.starts_with("flm_"),
        "control pair machine_token: {machine}"
    );
    assert!(
        paired.body["control_plane_url"].as_str().is_some(),
        "control pair control_plane_url: {}",
        paired.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after control pair: {}",
        got.body
    );
    assert_eq!(
        got.body["hostname"].as_str(),
        Some(unique_hn.as_str()),
        "control pair must persist the unique hostname: {}",
        got.body
    );
    assert_eq!(
        got.body["platform"].as_str(),
        Some("linux-x86_64"),
        "control pair must persist platform: {}",
        got.body
    );
    assert_ne!(
        got.body["status"].as_str(),
        Some("pending"),
        "control pair must leave pending: {}",
        got.body
    );

    let again = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": unique_hn,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "second consume of the used flp_ must still be 409: {} {}",
        again.status,
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or(&again.raw);
    assert!(
        again_err.contains("used"),
        "second consume must say used: {again_err}"
    );
    assert_pair_body_omits_machine(&again, "second consume of used flp_");
}

/// Browser session cookies (`fleet_access` JWT, `fleet_refresh` opaque)
/// and machine tokens (`flm_…`) are not pair tokens. Guest
/// `POST /api/worker-hosts/pair` requires JSON `{token: flp_…}` —
/// a live login must not mint `machine_token` or consume the pending row.
#[tokio::test]
async fn pair_cookie_jwt_and_flm_cannot_complete_pair() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("signup login must set fleet_refresh")
        .to_string();
    assert!(
        access.contains('.') && access.split('.').count() == 3,
        "fleet_access must be a JWT, got {access:?}"
    );
    assert!(
        !access.starts_with("flp_") && !access.starts_with("flm_"),
        "fleet_access JWT must not look like a pair/machine token: {access}"
    );
    assert!(
        !refresh.starts_with("flp_") && !refresh.starts_with("flm_"),
        "fleet_refresh must not look like a pair/machine token: {refresh}"
    );

    let (_other_hid, other_machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "already-paired", true, false).await;
    assert!(
        other_machine.starts_with("flm_"),
        "control host must mint a real machine token: {other_machine}"
    );

    let pending_name = format!("pair-cookie-jwt-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": pending_name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(
        created.body["host"]["status"], "pending",
        "create must stay pending: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );
    assert_host_still_pending(&app, &jar, &ws, &hid, "after create").await;

    let hostname = "cookie-jwt-must-not-pair";
    let omitted_token = json!({
        "hostname": hostname,
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false,
    });

    for (label, body) in [
        ("Cookie-only empty token", pair_body("", hostname)),
        ("Cookie-only whitespace token", pair_body("   ", hostname)),
    ] {
        let out = call(
            &app,
            "POST",
            "/api/worker-hosts/pair",
            Some(&jar),
            Some(body),
        )
        .await;
        assert_pair_rejected_as_cookie_jwt(&out, label, false);
    }
    let omitted = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        Some(omitted_token.clone()),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(&omitted, "Cookie-only omitted token key", true);
    assert_host_still_pending(&app, &jar, &ws, &hid, "after Cookie-only pair probes").await;

    for (label, token) in [
        (
            "Bearer <fleet_access JWT> + body token=JWT",
            access.as_str(),
        ),
        ("body token=<fleet_access JWT>", access.as_str()),
        ("body token=<fleet_refresh opaque>", refresh.as_str()),
        (
            "body token=<flm_ from another already-paired host>",
            other_machine.as_str(),
        ),
    ] {
        let use_bearer = label.starts_with("Bearer");
        let out = if use_bearer {
            call_inner(
                &app,
                "POST",
                "/api/worker-hosts/pair",
                None,
                Some(token),
                Some(pair_body(token, hostname)),
            )
            .await
        } else {
            call(
                &app,
                "POST",
                "/api/worker-hosts/pair",
                None,
                Some(pair_body(token, hostname)),
            )
            .await
        };
        assert_pair_rejected_as_cookie_jwt(&out, label, false);
    }

    let cookie_plus_jwt = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        Some(access.as_str()),
        Some(pair_body(&access, hostname)),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(
        &cookie_plus_jwt,
        "owner Cookie + Bearer JWT + body token=JWT",
        false,
    );

    let bearer_flm = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(other_machine.as_str()),
        Some(pair_body(&other_machine, hostname)),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(
        &bearer_flm,
        "Authorization Bearer flm_ + body token=flm_",
        false,
    );

    let cookie_plus_flm = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        Some(other_machine.as_str()),
        Some(pair_body(&other_machine, hostname)),
    )
    .await;
    assert_pair_rejected_as_cookie_jwt(
        &cookie_plus_flm,
        "owner Cookie + Bearer flm_ + body token=flm_",
        false,
    );
    assert_host_still_pending(&app, &jar, &ws, &hid, "after JWT/flm_ pair probes").await;

    let paired = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        None,
        Some(pair_body(&pair_token, hostname)),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "Cookie + real flp_ must still pair: {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    assert_eq!(paired.body["workspace_id"].as_str(), Some(ws.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token")
        .to_string();
    assert!(
        machine.starts_with("flm_"),
        "Cookie + real flp_ must return flm_: {machine}"
    );
    assert!(
        paired.body["control_plane_url"].as_str().is_some(),
        "paired payload control_plane_url: {}",
        paired.body
    );

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_hb, "Cookie-only /me/heartbeat (control)");

    let again = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, hostname)),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "second consume of the used flp_ must be 409: {} {}",
        again.status,
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or(&again.raw);
    assert!(
        again_err.contains("used"),
        "second consume must say used: {again_err}"
    );
    assert_pair_body_omits_machine(&again, "second consume of used flp_");
}

fn assert_pair_body_omits_machine(out: &HttpOut, what: &str) {
    for key in ["machine_token", "control_plane_url"] {
        assert!(
            !json_has_key(&out.body, key),
            "{what} must not include {key}: {}",
            out.body
        );
    }
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{what} must not include ok:true: {}",
        out.body
    );
}

fn assert_pair_rejected_as_cookie_jwt(out: &HttpOut, what: &str, allow_missing_field_422: bool) {
    if allow_missing_field_422 && out.status == StatusCode::UNPROCESSABLE_ENTITY {
        assert_pair_body_omits_machine(out, what);
        return;
    }
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401 invalid or expired pair token (422 missing field is ok only for an omitted token key), got {}: {}",
        out.status,
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("invalid or expired pair token"),
        "{what} must be 401 invalid or expired pair token, got {err:?}: {}",
        out.body
    );
    assert_pair_body_omits_machine(out, what);
}

async fn assert_host_still_pending(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    hid: &str,
    what: &str,
) {
    let got = call(
        app,
        "GET",
        &format!("{}/{hid}", hosts_path(ws)),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{what} get: {}", got.body);
    assert_eq!(
        got.body["status"], "pending",
        "{what} host must stay pending: {}",
        got.body
    );
    assert!(
        got.body["last_seen_at"].is_null(),
        "{what} last_seen_at must stay null: {}",
        got.body
    );
    assert!(
        !json_has_key(&got.body, "machine_token") && !json_has_key(&got.body, "pair_token"),
        "{what} GET must not mint/echo tokens: {}",
        got.body
    );
}

#[tokio::test]
async fn re_pair_same_hostname_reclaims_row() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "box" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"].as_str().expect("pair_token");
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "studio-pc",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);

    let del = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(del.status, StatusCode::OK, "remove: {}", del.body);

    let created2 = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "box-again" })),
    )
    .await;
    assert_eq!(
        created2.status,
        StatusCode::OK,
        "create again: {}",
        created2.body
    );
    let hid2 = created2.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let token2 = created2.body["pair_token"].as_str().expect("pair_token");
    let again = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": token2,
            "hostname": "studio-pc",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "re-pair: {}", again.body);
    assert_eq!(
        again.body["host_id"].as_str(),
        Some(hid.as_str()),
        "same hostname must reclaim the original row: {}",
        again.body
    );
    assert_ne!(hid2, hid, "second create is a new pending row");

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let hosts = listed.body["hosts"].as_array().expect("hosts");
    let live = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .expect("original row");
    assert_eq!(live["status"], "online");
    assert_eq!(live["machine_id"].as_str(), Some(hid.as_str()));
    if let Some(leftover) = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid2.as_str()))
    {
        assert_eq!(leftover["status"], "disabled");
    }
}

#[tokio::test]
async fn heartbeat_and_lease_empty() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "box" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"].as_str().unwrap();
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "box",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let machine = paired.body["machine_token"].as_str().unwrap();

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    assert_eq!(hb.body["host"]["status"], "online");
    assert_eq!(hb.body["effective"]["host"], true);
    assert_eq!(hb.body["effective"]["container"], false);
    assert_eq!(hb.body["effective"]["max_concurrent"], 4);

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert!(lease.body["claim"].is_null(), "empty lease: {}", lease.body);
}

#[tokio::test]
async fn lease_returns_enqueued_spawn_session() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "box" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"].as_str().unwrap();
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "box",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let machine = paired.body["machine_token"].as_str().unwrap().to_string();
    let host_id = paired.body["host_id"]
        .as_str()
        .expect("host_id")
        .to_string();
    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);

    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_uuid = Uuid::parse_str(&host_id).expect("host_id");
    let claim_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({
                "kind": "spawn_session",
                "session_id": session_id,
                "host_id": host_uuid,
                "worker_token": "job-token",
                "runtime": "host",
                "orchestrator_url": "http://127.0.0.1:8080",
                "env": { "FLEET_WORKER_TOKEN": "job-token" }
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("enqueue");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let claim = lease.body["claim"].as_object().expect("claim object");
    assert_eq!(claim["id"].as_str(), Some(claim_id.to_string().as_str()));
    assert!(claim["lease_token"].as_str().is_some());
    assert_eq!(claim["payload"]["runtime"], "host");
    assert_eq!(claim["payload"]["worker_token"], "job-token");

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": claim["lease_token"],
            "ok": true
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "complete: {}", done.body);
}

fn host_shows_updating(got: &Value) -> bool {
    got["software"]["updating"] == true
        || got.get("updating") == Some(&json!(true))
        || got["software_status"] == "updating"
}

/// catalog:list-updating-host-lease-null — Apply §7 heartbeats
/// `software.updating=true` and must stop new leases without PATCH
/// paused/drain (those drop list_assignable). `host_accepts_leases`
/// checks `!host.software_updating` (and job cap) on top of
/// `accepts_v1_leases()` — enabled && !paused && !drain && online &&
/// (effective host, or cluster + effective container). `accepts_v1_leases()`
/// itself does not read
/// software_updating. Do not drop the HTTP helper's updating check.
/// Client-only stop-claim is not fail-closed.
/// Distinct from list-paused-host-lease-null / list-paused-host-completes-
/// claimed-lease (paused/drain/enabled), list-hold-ready-mid-host-swap-no-local
/// (tick hold_ready), and list-apply-bumps-settings-gen (settings ack).
#[tokio::test]
async fn updating_host_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "updating-lease-box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let before = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "GET after pair+heartbeat: {}",
        before.body
    );
    assert_eq!(
        before.body["id"].as_str(),
        Some(hid.as_str()),
        "{}",
        before.body
    );
    assert_eq!(before.body["status"], "online", "{}", before.body);
    assert_eq!(before.body["paused"], false, "{}", before.body);
    assert_eq!(before.body["drain"], false, "{}", before.body);
    assert_eq!(before.body["enabled"], true, "{}", before.body);
    assert_ne!(
        before.body["status"], "disabled",
        "paired host must not start disabled: {}",
        before.body
    );

    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    assert_claim_unleased(&state, queued, "fresh enqueue before updating").await;

    let hb_updating = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": true
            }
        })),
    )
    .await;
    assert_eq!(
        hb_updating.status,
        StatusCode::OK,
        "heartbeat software.updating=true: {}",
        hb_updating.body
    );
    assert_eq!(
        hb_updating.body["host"]["paused"], false,
        "heartbeat updating=true must not persist paused: {}",
        hb_updating.body
    );
    assert_eq!(
        hb_updating.body["host"]["drain"], false,
        "heartbeat updating=true must not persist drain: {}",
        hb_updating.body
    );
    assert_ne!(
        hb_updating.body["host"]["status"], "disabled",
        "updating must not disable the host: {}",
        hb_updating.body
    );
    assert!(
        host_shows_updating(&hb_updating.body["host"]),
        "heartbeat envelope must show software.updating / updating pill: {}",
        hb_updating.body
    );

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after updating heartbeat: {}",
        got.body
    );
    assert!(
        host_shows_updating(&got.body),
        "GET host must show software.updating or updating pill: {}",
        got.body
    );
    assert_eq!(got.body["paused"], false, "GET paused: {}", got.body);
    assert_eq!(got.body["drain"], false, "GET drain: {}", got.body);
    assert_eq!(got.body["enabled"], true, "GET enabled: {}", got.body);
    assert_ne!(
        got.body["status"], "disabled",
        "GET status must not be disabled: {}",
        got.body
    );
    assert_eq!(got.body["status"], "online", "GET status: {}", got.body);

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "GET list after updating: {}",
        listed.body
    );
    let card = list_host_by_id(&listed, &hid, "GET list after updating heartbeat");
    assert!(
        host_shows_updating(card),
        "list card must show software.updating or updating pill: {card}"
    );
    assert_eq!(card["paused"], false, "list card paused: {card}");
    assert_eq!(card["drain"], false, "list card drain: {card}");
    assert_ne!(
        card["status"], "disabled",
        "list card must not be disabled: {card}"
    );

    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after updating heartbeat"));
    assert!(
        stored.software_updating,
        "heartbeat updating=true must persist software_updating: {stored:?}"
    );
    assert!(
        !stored.paused,
        "heartbeat updating=true must not persist paused=true: {stored:?}"
    );
    assert!(
        !stored.drain,
        "heartbeat updating=true must not persist drain=true: {stored:?}"
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "software.updating=true /me/lease");
    assert_claim_unleased(
        &state,
        queued,
        "updating host must not consume the queued spawn_session",
    )
    .await;

    let later = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&later, "later /me/lease poll while updating");
    assert_claim_unleased(
        &state,
        queued,
        "later poll while updating must still leave spawn ready",
    )
    .await;

    let assignable = worker_hosts_assignable(&state, &ws).await;
    assert!(
        assignable.contains(&host_id),
        "list_assignable / picker must still include an updating host (do not treat updating as paused): {assignable:?}"
    );
    let listed_assignable = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        listed_assignable.iter().any(|h| h.id == host_id),
        "list_assignable must still include updating hid {host_id}: {listed_assignable:?}"
    );
    let picker_hosts: Vec<fleet_orchestrator::store::AssignableHost> = listed_assignable
        .iter()
        .filter(|h| h.workspace_id == workspace_id)
        .map(fleet_orchestrator::store::AssignableHost::from)
        .collect();
    let picked =
        fleet_orchestrator::store::pick_assignable_runner(&picker_hosts, &Default::default());
    assert_eq!(
        picked.as_ref().map(|c| c.id()),
        Some(host_id),
        "picker must still include this updating host (do not treat updating as paused): {picked:?}"
    );

    let hb_clear = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": false
            }
        })),
    )
    .await;
    assert_eq!(
        hb_clear.status,
        StatusCode::OK,
        "heartbeat software.updating=false: {}",
        hb_clear.body
    );
    assert_eq!(
        hb_clear.body["host"]["software"]["updating"], false,
        "updating=false must clear the pill: {}",
        hb_clear.body
    );
    assert_eq!(hb_clear.body["host"]["paused"], false, "{}", hb_clear.body);
    assert_eq!(hb_clear.body["host"]["drain"], false, "{}", hb_clear.body);
    assert_eq!(
        hb_clear.body["host"]["status"], "online",
        "{}",
        hb_clear.body
    );

    let claimed = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        claimed.status,
        StatusCode::OK,
        "online + updating=false must lease: {}",
        claimed.body
    );
    let claim = claimed.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "online + updating=false must still lease (not a tautology that every lease is null): {}",
            claimed.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(queued.to_string().as_str()),
        "must hand out the still-queued spawn: {}",
        claimed.body
    );
    assert_eq!(claim["payload"]["kind"], "spawn_session");
    assert_eq!(
        claim["payload"]["host_id"].as_str(),
        Some(hid.as_str()),
        "claim host_id: {}",
        claimed.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", claimed.body))
        .to_string();
    Uuid::parse_str(&lease_token).unwrap_or_else(|e| panic!("lease_token UUID {lease_token}: {e}"));
    assert_claim_still_leased(
        &state,
        queued,
        &lease_token,
        "after updating=false /me/lease",
    )
    .await;

    // Updating mid-job must not 409 an already-claimed lease (same as pause-completes).
    let hb_mid = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": true
            }
        })),
    )
    .await;
    assert_eq!(
        hb_mid.status,
        StatusCode::OK,
        "heartbeat updating=true after claim: {}",
        hb_mid.body
    );
    assert_eq!(hb_mid.body["host"]["paused"], false, "{}", hb_mid.body);
    assert_eq!(hb_mid.body["host"]["drain"], false, "{}", hb_mid.body);
    assert_claim_still_leased(
        &state,
        queued,
        &lease_token,
        "updating must not drop the already-held lease",
    )
    .await;

    let leftover_while_updating = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let mid_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &mid_lease,
        "updating=true /me/lease after already-claimed job",
    );
    assert_claim_unleased(
        &state,
        leftover_while_updating,
        "updating must not consume a not-yet-leased spawn",
    )
    .await;

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{queued}/complete"),
        &machine,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete already-claimed job while updating must stay 200: {}",
        done.body
    );

    let hb_accepting = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": false
            }
        })),
    )
    .await;
    assert_eq!(
        hb_accepting.status,
        StatusCode::OK,
        "control heartbeat updating=false: {}",
        hb_accepting.body
    );
    assert_eq!(
        hb_accepting.body["host"]["software"]["updating"], false,
        "{}",
        hb_accepting.body
    );
    assert_eq!(
        hb_accepting.body["host"]["paused"], false,
        "{}",
        hb_accepting.body
    );
    assert_eq!(
        hb_accepting.body["host"]["drain"], false,
        "{}",
        hb_accepting.body
    );
    assert_eq!(
        hb_accepting.body["host"]["status"], "online",
        "{}",
        hb_accepting.body
    );

    let cookie_lease = call(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_cookie_rejected_as_machine(
        &cookie_lease,
        "cookie-only /me/lease (list-cookie-jwt-rejected-as-machine-token)",
    );
    assert_claim_unleased(
        &state,
        leftover_while_updating,
        "cookie-only /me/lease must not consume the queued spawn",
    )
    .await;

    let control = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "control /me/lease after updating=false: {}",
        control.body
    );
    let control_claim = control.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "online + updating=false must still lease (not a tautology that every lease is null): {}",
            control.body
        )
    });
    assert_eq!(
        control_claim["id"].as_str(),
        Some(leftover_while_updating.to_string().as_str()),
        "control must hand out the still-queued spawn: {}",
        control.body
    );
    let control_token = control_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", control.body))
        .to_string();
    let control_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{leftover_while_updating}/complete"),
        &machine,
        Some(json!({
            "lease_token": control_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        control_done.status,
        StatusCode::OK,
        "complete control claim: {}",
        control_done.body
    );

    let leftover = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let paused = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        paused.status,
        StatusCode::OK,
        "PATCH paused: {}",
        paused.body
    );
    assert_eq!(paused.body["paused"], true, "{}", paused.body);
    assert_eq!(paused.body["status"], "online", "{}", paused.body);
    let paused_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&paused_lease, "PATCH paused /me/lease");
    assert_claim_unleased(
        &state,
        leftover,
        "paused must not consume the queued spawn (list-paused-host-lease-null)",
    )
    .await;
}

/// Expired spawn lease returns to ready; the next `/me/lease` from the same
/// `flm_` can claim it again. `http_lease` (not a never-called
/// `reclaim_expired_leases` unit) must be on the path.
#[tokio::test]
async fn expired_lease_returns_to_ready_next_me_lease_claims_again() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let first = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "first lease: {}", first.body);
    let first_claim = first.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("first lease must return a claim: {}", first.body));
    assert_eq!(
        first_claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "first lease claim.id: {}",
        first.body
    );
    let old_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("first lease_token: {}", first.body))
        .to_string();
    Uuid::parse_str(&old_token)
        .unwrap_or_else(|e| panic!("first lease_token UUID {old_token}: {e}"));
    assert_lease_body_has_no_host_secrets(&first);

    let still_held = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&still_held, "second lease while still leased");
    let held = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get held claim")
        .unwrap_or_else(|| panic!("held claim {claim_id} missing"));
    assert_eq!(held.status, "leased", "control: still leased: {held:?}");
    assert_eq!(
        held.lease_token.map(|t| t.to_string()).as_deref(),
        Some(old_token.as_str()),
        "control: same lease_token while held: {held:?}"
    );

    let aged = sqlx::query(
        r#"
        UPDATE work_claims
        SET lease_expires_at = now() - interval '1 minute'
        WHERE id = $1
        "#,
    )
    .bind(claim_id)
    .execute(&state.db)
    .await
    .expect("backdate lease_expires_at");
    assert_eq!(
        aged.rows_affected(),
        1,
        "expected the leased row aged: {claim_id}"
    );
    let expired_still_leased: bool = sqlx::query_scalar(
        r#"
        SELECT status = 'leased' AND lease_expires_at < now()
        FROM work_claims
        WHERE id = $1
        "#,
    )
    .bind(claim_id)
    .fetch_one(&state.db)
    .await
    .expect("post-backdate expiry flags");
    assert!(
        expired_still_leased,
        "backdate must leave the row leased with lease_expires_at in the past"
    );

    let reclaimed = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        reclaimed.status,
        StatusCode::OK,
        "lease after expiry: {}",
        reclaimed.body
    );
    let again = reclaimed.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("expired lease must be claimable again: {}", reclaimed.body));
    assert_eq!(
        again["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "reclaimed claim.id must match the snapshot: {}",
        reclaimed.body
    );
    let new_token = again["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("new lease_token: {}", reclaimed.body))
        .to_string();
    Uuid::parse_str(&new_token).unwrap_or_else(|e| panic!("new lease_token UUID {new_token}: {e}"));
    assert_ne!(
        new_token, old_token,
        "reclaim must issue a new lease_token: {}",
        reclaimed.body
    );
    assert_lease_body_has_no_host_secrets(&reclaimed);

    let stale = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": old_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        stale.status,
        StatusCode::CONFLICT,
        "old lease_token after reclaim: {}",
        stale.body
    );
    let err = stale.body["error"].as_str().unwrap_or(&stale.raw);
    assert!(
        err.contains("lease not held"),
        "409 must say lease not held: {err}"
    );
    let after_stale = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get after stale complete")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after stale complete"));
    assert_ne!(
        after_stale.status, "done",
        "old token must not mark the claim done: {after_stale:?}"
    );
    assert_eq!(
        after_stale.status, "leased",
        "claim stays leased under the new token: {after_stale:?}"
    );
    assert_eq!(
        after_stale.lease_token.map(|t| t.to_string()).as_deref(),
        Some(new_token.as_str()),
        "stale complete must not clear the new lease_token: {after_stale:?}"
    );

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": new_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "new token complete: {}",
        done.body
    );

    let control_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let control = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "in-flight lease: {}",
        control.body
    );
    assert_eq!(
        control.body["claim"]["id"].as_str(),
        Some(control_id.to_string().as_str()),
        "control claim.id: {}",
        control.body
    );
    let control_token = control.body["claim"]["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", control.body))
        .to_string();
    Uuid::parse_str(&control_token)
        .unwrap_or_else(|e| panic!("control lease_token UUID {control_token}: {e}"));
    let control_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{control_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": control_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        control_done.status,
        StatusCode::OK,
        "in-flight job must still complete with its original token: {}",
        control_done.body
    );
}

fn assert_lease_body_has_no_host_secrets(out: &HttpOut) {
    assert!(
        !out.raw.contains("machine_token"),
        "lease body must not include machine_token: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("pair_token"),
        "lease body must not include pair_token: {}",
        out.raw
    );
}

fn listed_host<'a>(listed: &'a Value, hid: &str) -> &'a Value {
    listed["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(hid))
        .unwrap_or_else(|| panic!("host {hid} missing: {listed}"))
}

#[tokio::test]
async fn heartbeat_without_software_leaves_public_software_null() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;
    let hid = id.to_string();
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["package_versions"]["fleet-host"],
        fleet_server::package_versions::fleet_host_version(),
        "list must publish current package versions: {}",
        listed.body
    );
    assert_eq!(
        listed.body["package_versions"]["fleet-driver"],
        fleet_server::package_versions::fleet_driver_version(),
        "{}",
        listed.body
    );
    assert_ne!(listed.body["package_versions"]["fleet-host"], "0.0.0");
    assert!(
        listed.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .all(|h| h["kind"] != "control_plane"),
        "list must not synthesize a plane runner: {}",
        listed.body
    );
    let host = listed_host(&listed.body, &hid);
    assert!(
        host.get("software").is_none() || host["software"].is_null(),
        "old host must not invent a version: {}",
        host
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "software": { "git_sha": "abc123def456", "updating": false }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "software heartbeat: {}", hb.body);
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    let host = listed_host(&listed.body, &hid);
    assert_eq!(host["software"]["git_sha"], "abc123def456", "{}", host);

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 2,
            "hardware": { "cpu_cores": 8, "mem_total_bytes": 32000000000u64, "mem_used_pct": 41, "os": "linux 6.8 x86_64" }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "hardware heartbeat: {}", hb.body);
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    let host = listed_host(&listed.body, &hid);
    assert_eq!(host["current_jobs"], 2, "{}", host);
    assert_eq!(host["hardware"]["cpu_cores"], 8, "{}", host);
    assert_eq!(host["hardware"]["mem_used_pct"], 41, "{}", host);
}

/// ADMIN_TOKEN (`test-admin`) is /v1 admin only. Machine routes require
/// `Authorization: Bearer flm_…` — a valid admin secret must not heartbeat
/// or lease as every host.
#[tokio::test]
async fn admin_token_machine_cannot_authenticate_heartbeat_or_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after pair: {}", got.body);
    let last_seen = got.body["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("last_seen_at after pair: {}", got.body))
        .to_string();
    let stored_before = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after pair"));
    let seen_at = stored_before.last_seen_at;
    assert!(
        seen_at.is_some(),
        "paired+heartbeat host must have last_seen_at: {stored_before:?}"
    );
    let jobs_before = stored_before.current_jobs;

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    assert_claim_unleased(&state, claim_id, "enqueued spawn before admin probes").await;

    let hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 99,
        "hostname": "admin-must-not-touch"
    });
    let lease_body = Some(json!({}));
    let planted_complete = json!({
        "lease_token": Uuid::new_v4().to_string(),
        "ok": true
    });

    for token in [TEST_ADMIN_TOKEN, "", "x"] {
        let label = if token.is_empty() {
            "Bearer empty"
        } else if token == TEST_ADMIN_TOKEN {
            "Bearer test-admin"
        } else {
            "Bearer x"
        };
        let hb = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            token,
            Some(hb_body.clone()),
        )
        .await;
        assert_admin_rejected_as_machine(&hb, &format!("{label} /me/heartbeat"));

        let lease = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/lease",
            token,
            lease_body.clone(),
        )
        .await;
        assert_admin_rejected_as_machine(&lease, &format!("{label} /me/lease"));
    }

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(hb_body.clone()),
    )
    .await;
    assert_admin_rejected_as_machine(&cookie_hb, "Cookie-only /me/heartbeat");

    let after = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "get after admin probes: {}",
        after.body
    );
    assert_eq!(
        after.body["last_seen_at"].as_str(),
        Some(last_seen.as_str()),
        "ADMIN_TOKEN heartbeat must not touch last_seen_at: {}",
        after.body
    );
    assert_eq!(
        after.body["current_jobs"].as_i64().unwrap_or(-1),
        i64::from(jobs_before),
        "ADMIN_TOKEN heartbeat must not persist current_jobs: {}",
        after.body
    );
    let stored_after = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host after admin")
        .unwrap_or_else(|| panic!("host {host_id} missing after admin probes"));
    assert_eq!(
        stored_after.last_seen_at, seen_at,
        "ADMIN_TOKEN must not rewrite last_seen_at: {stored_after:?}"
    );
    assert_eq!(
        stored_after.current_jobs, jobs_before,
        "ADMIN_TOKEN must not rewrite current_jobs: {stored_after:?}"
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "ADMIN_TOKEN /me/lease must not consume the enqueued spawn",
    )
    .await;

    let planted = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        TEST_ADMIN_TOKEN,
        Some(planted_complete),
    )
    .await;
    assert_admin_rejected_as_machine(
        &planted,
        "Bearer test-admin /me/lease/{claim_id}/complete (planted)",
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "ADMIN_TOKEN complete must not finish an unleased spawn",
    )
    .await;

    let hb_ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb_ok.status,
        StatusCode::OK,
        "real flm_ heartbeat: {}",
        hb_ok.body
    );
    assert_eq!(hb_ok.body["host"]["id"].as_str(), Some(hid.as_str()));
    assert!(
        hb_ok.body.get("effective").is_some(),
        "real flm_ heartbeat returns effective: {}",
        hb_ok.body
    );

    let lease_ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease_ok.status,
        StatusCode::OK,
        "real flm_ lease: {}",
        lease_ok.body
    );
    let claim = lease_ok.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("real flm_ must lease the spawn: {}", lease_ok.body));
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "control claim.id: {}",
        lease_ok.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", lease_ok.body))
        .to_string();
    Uuid::parse_str(&lease_token)
        .unwrap_or_else(|e| panic!("control lease_token UUID {lease_token}: {e}"));

    let held = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get leased claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after real lease"));
    assert_eq!(held.status, "leased", "control: flm_ leased: {held:?}");

    let admin_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        TEST_ADMIN_TOKEN,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_admin_rejected_as_machine(
        &admin_done,
        "Bearer test-admin /me/lease/{claim_id}/complete (real lease_token)",
    );
    let still_held = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get after admin complete")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after admin complete"));
    assert_ne!(
        still_held.status, "done",
        "ADMIN_TOKEN complete must not mark the claim finished: {still_held:?}"
    );
    assert_eq!(
        still_held.status, "leased",
        "claim stays leased after ADMIN_TOKEN complete: {still_held:?}"
    );
    assert_eq!(
        still_held.lease_token.map(|t| t.to_string()).as_deref(),
        Some(lease_token.as_str()),
        "ADMIN_TOKEN complete must not clear lease_token: {still_held:?}"
    );

    let owner_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        owner_done.status,
        StatusCode::OK,
        "real flm_ complete after admin 401: {}",
        owner_done.body
    );

    let created = call_bearer(
        &app,
        "POST",
        "/v1/projects",
        TEST_ADMIN_TOKEN,
        Some(json!({
            "name": format!("v1-admin-{}", Uuid::new_v4()),
            "max_agents": 2
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN POST /v1/projects must 201 (401s above are not admin disabled): {}",
        created.body
    );

    let cookie_again = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(hb_body),
    )
    .await;
    assert_admin_rejected_as_machine(&cookie_again, "Cookie-only /me/heartbeat (control)");
}

/// Browser session cookies (`fleet_access` JWT, `fleet_refresh` opaque) are
/// not machine tokens. `/me/heartbeat` and `/me/lease` require
/// `Authorization: Bearer flm_…` — a live login must not heartbeat or lease.
#[tokio::test]
async fn cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("signup login must set fleet_refresh")
        .to_string();
    assert!(
        access.contains('.') && access.split('.').count() == 3,
        "fleet_access must be a JWT, got {access:?}"
    );
    assert!(
        !access.starts_with("flm_"),
        "fleet_access JWT must not look like a machine token: {access}"
    );
    assert!(
        !refresh.starts_with("flm_"),
        "fleet_refresh must not look like a machine token: {refresh}"
    );

    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::OK,
        "owner session must be live before /me probes: {}",
        me.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after pair: {}", got.body);
    let last_seen = got.body["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("last_seen_at after pair: {}", got.body))
        .to_string();
    let stored_before = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after pair"));
    let seen_at = stored_before.last_seen_at;
    assert!(
        seen_at.is_some(),
        "paired+heartbeat host must have last_seen_at: {stored_before:?}"
    );
    let jobs_before = stored_before.current_jobs;

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    assert_claim_unleased(&state, claim_id, "enqueued spawn before cookie/JWT probes").await;

    let hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 99,
        "hostname": "cookie-jwt-must-not-touch"
    });
    let lease_body = Some(json!({}));

    for (method, uri, body) in [
        (
            "POST",
            "/api/worker-hosts/me/heartbeat",
            Some(hb_body.clone()),
        ),
        ("POST", "/api/worker-hosts/me/lease", lease_body.clone()),
    ] {
        let cookie_only = call(&app, method, uri, Some(&jar), body.clone()).await;
        assert_cookie_rejected_as_machine(
            &cookie_only,
            &format!("owner Cookie-only (no Authorization) {uri}"),
        );
    }

    for (token, label) in [
        (access.as_str(), "Bearer <fleet_access JWT>"),
        (refresh.as_str(), "Bearer <fleet_refresh opaque>"),
    ] {
        let hb = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            token,
            Some(hb_body.clone()),
        )
        .await;
        assert_user_jwt_rejected_as_machine(&hb, &format!("{label} /me/heartbeat"));

        let lease = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/lease",
            token,
            lease_body.clone(),
        )
        .await;
        assert_user_jwt_rejected_as_machine(&lease, &format!("{label} /me/lease"));

        let cookie_plus = call_inner(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            Some(&jar),
            Some(token),
            Some(hb_body.clone()),
        )
        .await;
        assert_user_jwt_rejected_as_machine(
            &cookie_plus,
            &format!("owner Cookie + {label} /me/heartbeat"),
        );

        let cookie_plus_lease = call_inner(
            &app,
            "POST",
            "/api/worker-hosts/me/lease",
            Some(&jar),
            Some(token),
            lease_body.clone(),
        )
        .await;
        assert_user_jwt_rejected_as_machine(
            &cookie_plus_lease,
            &format!("owner Cookie + {label} /me/lease"),
        );
    }

    let after = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "get after cookie/JWT probes: {}",
        after.body
    );
    assert_eq!(
        after.body["last_seen_at"].as_str(),
        Some(last_seen.as_str()),
        "cookie/JWT heartbeat must not touch last_seen_at: {}",
        after.body
    );
    assert_eq!(
        after.body["current_jobs"].as_i64().unwrap_or(-1),
        i64::from(jobs_before),
        "cookie/JWT heartbeat must not persist current_jobs: {}",
        after.body
    );
    let stored_after = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host after cookie/JWT")
        .unwrap_or_else(|| panic!("host {host_id} missing after cookie/JWT probes"));
    assert_eq!(
        stored_after.last_seen_at, seen_at,
        "cookie/JWT must not rewrite last_seen_at: {stored_after:?}"
    );
    assert_eq!(
        stored_after.current_jobs, jobs_before,
        "cookie/JWT must not rewrite current_jobs: {stored_after:?}"
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "cookie/JWT /me/lease must not consume the enqueued spawn",
    )
    .await;

    let cookie_plus_flm = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(machine.as_str()),
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        cookie_plus_flm.status,
        StatusCode::OK,
        "Cookie + Bearer flm_ heartbeat: {}",
        cookie_plus_flm.body
    );
    assert_eq!(
        cookie_plus_flm.body["host"]["id"].as_str(),
        Some(hid.as_str()),
        "Cookie + Bearer flm_ must use the machine token, not the session: {}",
        cookie_plus_flm.body
    );
    assert!(
        cookie_plus_flm.body.get("effective").is_some(),
        "Cookie + Bearer flm_ heartbeat returns effective: {}",
        cookie_plus_flm.body
    );

    let hb_ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb_ok.status,
        StatusCode::OK,
        "real flm_ heartbeat (no cookie): {}",
        hb_ok.body
    );
    assert_eq!(hb_ok.body["host"]["id"].as_str(), Some(hid.as_str()));
    assert!(
        hb_ok.body.get("effective").is_some(),
        "real flm_ heartbeat returns effective: {}",
        hb_ok.body
    );

    let lease_ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease_ok.status,
        StatusCode::OK,
        "real flm_ lease: {}",
        lease_ok.body
    );
    let claim = lease_ok.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("real flm_ must lease the spawn: {}", lease_ok.body));
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "control claim.id: {}",
        lease_ok.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", lease_ok.body))
        .to_string();
    Uuid::parse_str(&lease_token)
        .unwrap_or_else(|e| panic!("control lease_token UUID {lease_token}: {e}"));

    let held = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get leased claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after real lease"));
    assert_eq!(held.status, "leased", "control: flm_ leased: {held:?}");

    let me_after = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me_after.status,
        StatusCode::OK,
        "owner session must still be live after /me probes: {}",
        me_after.body
    );
}

/// Pair token (`flp_…`) is the one-shot install command. Machine routes
/// require `Authorization: Bearer flm_…` — anyone who saw the UI pair
/// token must not heartbeat or lease as that host without pairing.
#[tokio::test]
async fn pair_token_machine_cannot_authenticate_heartbeat_or_lease() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("pair-as-machine-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    assert_eq!(
        created.body["host"]["status"], "pending",
        "create host must start pending: {}",
        created.body
    );
    assert!(
        created.body["host"]["last_seen_at"].is_null(),
        "create host last_seen_at must be null: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );
    assert!(
        !pair_token.starts_with("flm_"),
        "pair_token must not look like a machine token: {pair_token}"
    );

    let hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 99,
        "hostname": "pair-token-must-not-heartbeat"
    });
    let lease_body = Some(json!({}));

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &pair_token,
        Some(hb_body.clone()),
    )
    .await;
    assert_pair_token_rejected_as_machine(&hb, "Bearer <flp_> /me/heartbeat");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &pair_token,
        lease_body.clone(),
    )
    .await;
    assert_pair_token_rejected_as_machine(&lease, "Bearer <flp_> /me/lease");

    let cookie_plus_hb = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(pair_token.as_str()),
        Some(hb_body.clone()),
    )
    .await;
    assert_pair_token_rejected_as_machine(
        &cookie_plus_hb,
        "owner Cookie + Bearer <flp_> /me/heartbeat",
    );

    let cookie_plus_lease = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(pair_token.as_str()),
        lease_body,
    )
    .await;
    assert_pair_token_rejected_as_machine(
        &cookie_plus_lease,
        "owner Cookie + Bearer <flp_> /me/lease",
    );

    assert_host_still_pending(&app, &jar, &ws, &hid, "after flp_ machine probes").await;
    let still = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "get after flp_ machine probes: {}",
        still.body
    );
    assert_ne!(
        still.body["status"].as_str(),
        Some("online"),
        "flp_ heartbeat must not mark the host online: {}",
        still.body
    );
    assert_ne!(
        still.body["hostname"].as_str(),
        Some("pair-token-must-not-heartbeat"),
        "flp_ heartbeat must not persist hostname: {}",
        still.body
    );

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(hb_body.clone()),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_hb, "Cookie-only /me/heartbeat");

    let hostname = format!("pair-as-machine-hn-{}", Uuid::new_v4());
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "unused flp_ POST /pair after /me probes must still 200 (heartbeat must not consume it): {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    assert_eq!(paired.body["workspace_id"].as_str(), Some(ws.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control pair must return machine_token: {}", paired.body))
        .to_string();
    assert!(
        machine.starts_with("flm_"),
        "control pair machine_token: {machine}"
    );
    assert_ne!(
        machine, pair_token,
        "machine token must not be the pair token"
    );

    let after_pair = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        after_pair.status,
        StatusCode::OK,
        "get after control pair: {}",
        after_pair.body
    );
    let last_seen = after_pair.body["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("last_seen_at after pair: {}", after_pair.body))
        .to_string();
    let jobs_after_pair = after_pair.body["current_jobs"].as_i64().unwrap_or(-1);

    let used_flp_hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &pair_token,
        Some(hb_body.clone()),
    )
    .await;
    assert_pair_token_rejected_as_machine(&used_flp_hb, "used flp_ /me/heartbeat after pair");

    let after_used = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        after_used.status,
        StatusCode::OK,
        "get after used flp_ heartbeat: {}",
        after_used.body
    );
    assert_eq!(
        after_used.body["last_seen_at"].as_str(),
        Some(last_seen.as_str()),
        "used flp_ heartbeat must not touch last_seen_at: {}",
        after_used.body
    );
    assert_eq!(
        after_used.body["current_jobs"].as_i64().unwrap_or(-1),
        jobs_after_pair,
        "used flp_ heartbeat must not persist current_jobs: {}",
        after_used.body
    );
    assert_ne!(
        after_used.body["hostname"].as_str(),
        Some("pair-token-must-not-heartbeat"),
        "used flp_ heartbeat must not persist hostname: {}",
        after_used.body
    );

    let hb_ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "hostname": hostname,
        })),
    )
    .await;
    assert_eq!(
        hb_ok.status,
        StatusCode::OK,
        "real flm_ heartbeat: {}",
        hb_ok.body
    );
    assert_eq!(
        hb_ok.body["host"]["id"].as_str(),
        Some(hid.as_str()),
        "real flm_ heartbeat must use the machine token: {}",
        hb_ok.body
    );
    assert!(
        hb_ok.body.get("effective").is_some(),
        "real flm_ heartbeat returns effective: {}",
        hb_ok.body
    );

    let cookie_after = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(hb_body),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_after, "Cookie-only /me/heartbeat (control)");
}

fn assert_machine_route_omits_claim_payload(out: &HttpOut, what: &str) {
    for key in ["host", "effective", "claim", "lease_token"] {
        assert!(
            out.body.get(key).is_none(),
            "{what} must not include {key}: {}",
            out.body
        );
    }
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{what} must not include ok:true: {}",
        out.body
    );
    let dumped = out.body.to_string();
    assert!(
        !dumped.contains("lease_token"),
        "{what} must not include lease_token: {}",
        out.body
    );
    assert!(
        !dumped.contains("spawn_session"),
        "{what} must not include a claim payload: {}",
        out.body
    );
}

fn assert_cookie_rejected_as_machine(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert_eq!(
        err, "not authenticated",
        "{what} must be 401 not authenticated (cookie is not a machine Bearer), got {err:?}: {}",
        out.body
    );
    assert!(
        !err.contains("invalid machine token"),
        "{what} must not report invalid machine token (there is no Authorization): {err:?}: {}",
        out.body
    );
    assert_machine_route_omits_claim_payload(out, what);
}

fn assert_user_jwt_rejected_as_machine(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert_eq!(
        err, "invalid machine token",
        "{what} must be 401 invalid machine token (user JWT/refresh is not flm_), got {err:?}: {}",
        out.body
    );
    assert_machine_route_omits_claim_payload(out, what);
}

fn assert_pair_token_rejected_as_machine(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert_eq!(
        err, "invalid machine token",
        "{what} must be 401 invalid machine token (flp_ pair token is not flm_), got {err:?}: {}",
        out.body
    );
    assert_machine_route_omits_claim_payload(out, what);
}

fn assert_admin_rejected_as_machine(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("invalid machine token") || err.contains("not authenticated"),
        "{what} must be 401 invalid machine token / not authenticated, got {err:?}: {}",
        out.body
    );
    for key in ["host", "effective", "claim", "lease_token"] {
        assert!(
            out.body.get(key).is_none(),
            "{what} must not include {key}: {}",
            out.body
        );
    }
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{what} must not include ok:true: {}",
        out.body
    );
    let dumped = out.body.to_string();
    assert!(
        !dumped.contains("lease_token"),
        "{what} must not include lease_token: {}",
        out.body
    );
    assert!(
        !dumped.contains("spawn_session"),
        "{what} must not include a claim payload: {}",
        out.body
    );
}

async fn create_pair_and_heartbeat(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    name: &str,
    advertised_host: bool,
    advertised_container: bool,
) -> (Uuid, String) {
    let created = call(
        app,
        "POST",
        &hosts_path(ws),
        Some(jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"].as_str().unwrap();
    let paired = call(
        app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": name,
            "platform": "linux-x86_64",
            "advertised_host": advertised_host,
            "advertised_container": advertised_container,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let host_id = Uuid::parse_str(paired.body["host_id"].as_str().expect("host_id")).expect("uuid");
    let machine = paired.body["machine_token"].as_str().unwrap().to_string();
    let hb = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": advertised_host,
            "advertised_container": advertised_container,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    (host_id, machine)
}

/// Official CLI inventory on heartbeat must never persist or echo tokens.
/// `sanitize_heartbeat_agents` is the product gate; this drives the oneshot
/// (GET list/detail, heartbeat envelope, stored JSONB, `/chat/status`).
const HEARTBEAT_AGENT_SECRET_NEEDLES: &[&str] = &[
    "unique-secret",
    "sk-test-unique",
    "unique-at",
    "Bearer unique",
];

const HEARTBEAT_AGENT_SECRET_KEYS: &[&str] = &["token", "api_key", "access_token", "Authorization"];

fn assert_raw_omits_agent_secrets(raw: &str, what: &str) {
    for needle in HEARTBEAT_AGENT_SECRET_NEEDLES {
        assert!(
            !raw.contains(needle),
            "{what} must not contain {needle}: {raw}"
        );
    }
}

fn assert_http_omits_agent_secrets(out: &HttpOut, what: &str) {
    assert_eq!(out.status, StatusCode::OK, "{what} status: {}", out.body);
    assert_raw_omits_agent_secrets(&out.raw, what);
    for needle in HEARTBEAT_AGENT_SECRET_NEEDLES {
        assert!(
            !json_contains_str(&out.body, needle),
            "{what} leaked {needle}: {}",
            out.body
        );
    }
}

fn assert_agent_objects_omit_secret_keys(agents: &Value, what: &str) {
    let arr = agents.as_array().unwrap_or_else(|| {
        panic!("{what} agents must be an array: {agents}");
    });
    for agent in arr {
        for key in HEARTBEAT_AGENT_SECRET_KEYS {
            assert!(
                agent.get(*key).is_none(),
                "{what} agent object must not have {key}: {agent}"
            );
        }
        for needle in HEARTBEAT_AGENT_SECRET_NEEDLES {
            assert!(
                !json_contains_str(agent, needle),
                "{what} agent object leaked {needle}: {agent}"
            );
        }
    }
}

fn assert_grok_inventory(agents: &Value, what: &str) {
    let arr = agents.as_array().unwrap_or_else(|| {
        panic!("{what} agents must be an array: {agents}");
    });
    let grok = arr
        .iter()
        .find(|a| a["id"].as_str() == Some("grok"))
        .unwrap_or_else(|| panic!("{what} agents[] must include id=grok: {agents}"));
    assert_eq!(
        grok["name"].as_str(),
        Some("Grok"),
        "{what} grok.name: {grok}"
    );
    let models = grok["models"].as_array().unwrap_or_else(|| {
        panic!("{what} grok.models must be an array: {grok}");
    });
    assert!(
        models.iter().any(|m| m.as_str() == Some("grok-4")),
        "{what} grok.models must include grok-4: {grok}"
    );
    assert_agent_objects_omit_secret_keys(agents, what);
}

fn list_host_by_id<'a>(listed: &'a HttpOut, hid: &str, what: &str) -> &'a Value {
    listed.body["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["id"].as_str() == Some(hid))
        .unwrap_or_else(|| panic!("{what} list must include host {hid}: {}", listed.body))
}

/// POST `/me/heartbeat` with token-bearing `agents` must store only the
/// official CLI fields. Later omit-agents COALESCE keeps grok; cookie-only
/// `/me/heartbeat` stays 401.
#[tokio::test]
async fn heartbeat_agents_omit_tokens_from_persist_and_reads() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "omit-tok-box", true, false).await;
    let hid = host_id.to_string();
    let stamp = Uuid::new_v4();
    let token = format!("unique-secret-{stamp}");
    let api_key = format!("sk-test-unique-{stamp}");
    let access_token = format!("unique-at-{stamp}");
    let authorization = format!("Bearer unique {stamp}");

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2,
            "agents": [{
                "id": "grok",
                "name": "Grok",
                "installed": true,
                "logged_in": true,
                "models": ["grok-4"],
                "token": token,
                "api_key": api_key,
                "access_token": access_token,
                "Authorization": authorization,
            }],
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat with agents: {}",
        hb.body
    );
    assert_http_omits_agent_secrets(&hb, "heartbeat body / raw envelope");
    assert!(
        hb.body.get("host").is_some(),
        "heartbeat envelope must include host: {}",
        hb.body
    );
    assert_grok_inventory(&hb.body["host"]["agents"], "heartbeat host.agents");

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_http_omits_agent_secrets(&got, &format!("GET /worker-hosts/{hid}"));
    assert_grok_inventory(&got.body["agents"], "GET detail agents");

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_http_omits_agent_secrets(&listed, "GET worker-hosts list");
    let listed_host = list_host_by_id(&listed, &hid, "GET list after secret heartbeat");
    assert_grok_inventory(&listed_host["agents"], "GET list host.agents");

    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after secret heartbeat"));
    let stored_raw = stored.agent_runtimes.to_string();
    assert_raw_omits_agent_secrets(
        &stored_raw,
        "persisted agent_runtimes JSONB (storage, not as_public serde drop)",
    );
    assert_grok_inventory(&stored.agent_runtimes, "persisted agent_runtimes JSONB");

    let status = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/chat/status?runner_id={hid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_http_omits_agent_secrets(&status, &format!("GET /chat/status?runner_id={hid}"));
    let providers = status.body["providers"]
        .as_array()
        .unwrap_or_else(|| panic!("chat/status providers: {}", status.body));
    assert!(
        providers.iter().any(|p| p["id"].as_str() == Some("grok")),
        "chat/status must still advertise grok from sanitized inventory: {}",
        status.body
    );
    for provider in providers {
        for key in HEARTBEAT_AGENT_SECRET_KEYS {
            assert!(
                provider.get(*key).is_none(),
                "chat/status provider must not have {key}: {provider}"
            );
        }
    }

    let omit = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(
        omit.status,
        StatusCode::OK,
        "omit-agents heartbeat (COALESCE): {}",
        omit.body
    );
    assert_http_omits_agent_secrets(&omit, "omit-agents heartbeat envelope");
    assert_grok_inventory(
        &omit.body["host"]["agents"],
        "omit-agents heartbeat must COALESCE and keep grok",
    );

    let got_after = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_http_omits_agent_secrets(&got_after, "GET detail after omit-agents");
    assert_grok_inventory(
        &got_after.body["agents"],
        "GET detail after omit-agents COALESCE",
    );
    let stored_after = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host after omit")
        .unwrap_or_else(|| panic!("host {host_id} missing after omit-agents"));
    assert_grok_inventory(
        &stored_after.agent_runtimes,
        "persisted agent_runtimes after omit-agents COALESCE",
    );

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "agents": [{
                "id": "must-not-store",
                "token": token,
                "api_key": api_key,
            }],
        })),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_hb, "Cookie-only /me/heartbeat");
    assert_raw_omits_agent_secrets(&cookie_hb.raw, "cookie-only /me/heartbeat reject body");
    let still = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_http_omits_agent_secrets(&still, "GET detail after cookie-only heartbeat");
    assert_grok_inventory(
        &still.body["agents"],
        "cookie-only heartbeat must not wipe COALESCE grok inventory",
    );
}

async fn enqueue_spawn_for_host(state: &AppState, workspace_id: Uuid, host_id: Uuid) -> Uuid {
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({
                "kind": "spawn_session",
                "session_id": Uuid::new_v4(),
                "host_id": host_id,
                "worker_token": "job-token",
                "runtime": "host",
                "orchestrator_url": "http://127.0.0.1:8080",
                "env": { "FLEET_WORKER_TOKEN": "job-token" }
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("enqueue");
    claim_id
}

async fn enqueue_chat_turn_for_host(state: &AppState, workspace_id: Uuid, host_id: Uuid) -> Uuid {
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "chat_turn".into(),
            payload: json!({
                "kind": "chat_turn",
                "host_id": host_id,
                "provider": "grok",
                "model": "grok-4",
                "prompt": "hi",
                "timeout_s": 180,
            }),
            max_attempts: 1,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("enqueue chat_turn");
    claim_id
}

async fn assert_claim_unleased(state: &AppState, claim_id: Uuid, what: &str) {
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("{what}: claim {claim_id} missing"));
    assert_eq!(row.status, "ready", "{what} must stay unleased: {row:?}");
    assert!(
        row.lease_token.is_none(),
        "{what} must not have a lease_token: {row:?}"
    );
}

async fn assert_claim_still_leased(
    state: &AppState,
    claim_id: Uuid,
    lease_token: &str,
    what: &str,
) {
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("{what}: claim {claim_id} missing"));
    assert_ne!(
        row.status, "done",
        "{what} must not mark the claim finished: {row:?}"
    );
    assert_eq!(row.status, "leased", "{what}: claim stays leased: {row:?}");
    assert_eq!(
        row.lease_token.map(|t| t.to_string()).as_deref(),
        Some(lease_token),
        "{what} must not clear lease_token: {row:?}"
    );
}

fn assert_lease_claim_null(lease: &HttpOut, what: &str) {
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "{what} status: {}",
        lease.body
    );
    assert!(
        lease.body["claim"].is_null(),
        "{what} must be 200 {{claim: null}}, not a spawn_session lease: {}",
        lease.body
    );
    assert!(
        lease.body.get("lease_token").is_none() || lease.body["lease_token"].is_null(),
        "{what} must not return a lease_token: {}",
        lease.body
    );
    let dumped = lease.body.to_string();
    assert!(
        !dumped.contains("lease_token"),
        "{what} must not return a lease_token / spawn_session payload: {}",
        lease.body
    );
    assert!(
        !dumped.contains("spawn_session"),
        "{what} must not return a spawn_session payload: {}",
        lease.body
    );
    assert!(
        !dumped.contains("chat_turn"),
        "{what} must not return a chat_turn payload: {}",
        lease.body
    );
}

fn assert_host_still_online_accepting(got: &Value, hid: &str, what: &str) {
    assert_eq!(got["id"].as_str(), Some(hid), "{what} id: {got}");
    assert_eq!(got["status"], "online", "{what} must stay online: {got}");
    assert_eq!(got["paused"], false, "{what} paused unchanged: {got}");
    assert_eq!(got["drain"], false, "{what} drain unchanged: {got}");
    assert_eq!(got["enabled"], true, "{what} enabled unchanged: {got}");
    assert_eq!(
        got["allow_host"], true,
        "{what} allow_host still true (this lock is advertised_host, not PATCH allow_host): {got}"
    );
    let raw = got["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("{what} last_seen_at: {got}"));
    let seen = chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("{what} last_seen_at parse {raw}: {e}"))
        .with_timezone(&chrono::Utc);
    let age = chrono::Utc::now().signed_duration_since(seen);
    assert!(
        age < chrono::Duration::seconds(30) && age > chrono::Duration::seconds(-5),
        "{what} last_seen must be recent ({age}): {got}"
    );
}

/// Production tick is `select_assignable_runner` (host-only). A box that
/// stopped advertising host mode — even if it still offers container —
/// must not get a new session.
async fn assert_tick_does_not_assign(
    state: &AppState,
    workspace_id: Uuid,
    host_id: Uuid,
    what: &str,
) {
    if let Some(chosen) =
        fleet_orchestrator::store::select_assignable_runner(&state.orch.store, workspace_id).await
    {
        assert_ne!(
            chosen.id(),
            host_id,
            "{what}: a later tick must not assign a new session to that host: {chosen:?}"
        );
        assert_eq!(
            chosen.runtime,
            fleet_orchestrator::store::HostRuntime::Host,
            "{what}: v1 tick must never emit container/k3d: {chosen:?}"
        );
    }
    let listed = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let hosts: Vec<fleet_orchestrator::store::AssignableHost> = listed
        .iter()
        .map(fleet_orchestrator::store::AssignableHost::from)
        .collect();
    let picked = fleet_orchestrator::store::pick_assignable_runner(&hosts, &Default::default());
    assert!(
        picked.as_ref().map(|c| c.id()) != Some(host_id),
        "{what}: tick picker must not assign a box that only offers unsupported container: {picked:?}"
    );
}

/// Heartbeat advertised_host=false makes /me/lease claim:null even if
/// advertised_container is true. v1 is host mode only — container / k3d
/// is not a job runtime. Host stays online (not paused/revoked).
#[tokio::test]
async fn advertised_host_false_lease_null_even_if_advertised_container_true() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "adv-host-off", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "first lease: {}", lease.body);
    let claim = lease.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("online host must claim spawn_session: {}", lease.body));
    assert_eq!(claim["id"].as_str(), Some(claim_id.to_string().as_str()));
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", lease.body))
        .to_string();
    assert_eq!(claim["payload"]["kind"], "spawn_session");
    assert_eq!(
        claim["payload"]["host_id"].as_str(),
        Some(hid.as_str()),
        "snapshot hid+claim: {}",
        lease.body
    );

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "complete: {}", done.body);

    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let before = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "get before flip: {}",
        before.body
    );
    assert_eq!(before.body["advertised_host"], true, "{}", before.body);
    assert_eq!(
        before.body["advertised_container"], false,
        "{}",
        before.body
    );
    assert_eq!(
        before.body["allow_container"], true,
        "create-host defaults leave allow_container=true (this is the v1 hole): {}",
        before.body
    );
    assert_host_still_online_accepting(&before.body, &hid, "before advertised_host flip");

    let hb_container_only = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": false,
            "advertised_container": true,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb_container_only.status,
        StatusCode::OK,
        "heartbeat container-only: {}",
        hb_container_only.body
    );
    assert_eq!(
        hb_container_only.body["host"]["status"], "online",
        "heartbeat must not take the host offline: {}",
        hb_container_only.body
    );
    assert_eq!(
        hb_container_only.body["host"]["advertised_host"], false,
        "{}",
        hb_container_only.body
    );
    assert_eq!(
        hb_container_only.body["host"]["advertised_container"], true,
        "{}",
        hb_container_only.body
    );
    assert_eq!(
        hb_container_only.body["effective"]["host"], false,
        "effective host is allow_host && advertised_host: {}",
        hb_container_only.body
    );
    assert_eq!(
        hb_container_only.body["effective"]["container"], true,
        "container stays effective (allow_container && advertised_container) but is not a v1 lease path: {}",
        hb_container_only.body
    );

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "get after container-only: {}",
        got.body
    );
    assert_eq!(got.body["advertised_host"], false, "{}", got.body);
    assert_eq!(got.body["advertised_container"], true, "{}", got.body);
    assert_host_still_online_accepting(
        &got.body,
        &hid,
        "after advertised_host=false advertised_container=true",
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &lease,
        "advertised_host=false advertised_container=true /me/lease",
    );

    let chat_queued = enqueue_chat_turn_for_host(&state, workspace_id, host_id).await;
    let chat_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &chat_lease,
        "advertised_host=false must not claim chat_turn either",
    );
    assert_claim_unleased(
        &state,
        queued,
        "advertised_host=false advertised_container=true must not consume the queued spawn",
    )
    .await;
    assert_claim_unleased(
        &state,
        chat_queued,
        "advertised_host=false advertised_container=true must not consume the queued chat_turn",
    )
    .await;
    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "after advertised_host=false advertised_container=true",
    )
    .await;

    let hb_both_false = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": false,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb_both_false.status,
        StatusCode::OK,
        "heartbeat both false: {}",
        hb_both_false.body
    );
    assert_eq!(
        hb_both_false.body["host"]["status"], "online",
        "both-false heartbeat still online: {}",
        hb_both_false.body
    );

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "get after both false: {}",
        got.body
    );
    assert_eq!(got.body["advertised_host"], false, "{}", got.body);
    assert_eq!(got.body["advertised_container"], false, "{}", got.body);
    assert_host_still_online_accepting(&got.body, &hid, "after both advertised flags false");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "both advertised flags false /me/lease");

    let later = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&later, "later /me/lease poll after advertised_host=false");

    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "after both advertised flags false",
    )
    .await;
    assert_claim_unleased(
        &state,
        queued,
        "advertised_host=false must not consume the queued spawn",
    )
    .await;
    assert_claim_unleased(
        &state,
        chat_queued,
        "advertised_host=false must not consume the queued chat_turn",
    )
    .await;

    let hb_host_again = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb_host_again.status,
        StatusCode::OK,
        "restore advertised_host: {}",
        hb_host_again.body
    );
    assert_eq!(
        hb_host_again.body["effective"]["host"], true,
        "restored effective host: {}",
        hb_host_again.body
    );

    let cookie_lease = call(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        cookie_lease.status,
        StatusCode::UNAUTHORIZED,
        "cookie-only /me/lease must stay 401: {}",
        cookie_lease.body
    );
    assert!(
        cookie_lease.body.get("claim").is_none() || cookie_lease.body["claim"].is_null(),
        "cookie-only lease must not return a claim: {}",
        cookie_lease.body
    );
    assert!(
        !cookie_lease.body.to_string().contains("lease_token"),
        "cookie-only lease must not return a lease_token: {}",
        cookie_lease.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "restored advertised_host lease: {}",
        lease.body
    );
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "advertised_host=true must be able to claim again (not a tautology that every lease is null): {}",
            lease.body
        )
    });
    // chat_turn is leased before spawn_session.
    assert_eq!(
        claim["id"].as_str(),
        Some(chat_queued.to_string().as_str()),
        "restored host must claim the still-queued chat_turn first: {}",
        lease.body
    );
    let restore_chat_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token after restore (chat): {}", lease.body))
        .to_string();

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{chat_queued}/complete"),
        &machine,
        Some(json!({
            "lease_token": restore_chat_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete restored chat_turn: {}",
        done.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "restored advertised_host spawn lease: {}",
        lease.body
    );
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "advertised_host=true must claim the still-queued spawn after chat_turn: {}",
            lease.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(queued.to_string().as_str()),
        "restored host must claim the still-queued spawn: {}",
        lease.body
    );
    let restore_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token after restore: {}", lease.body))
        .to_string();

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{queued}/complete"),
        &machine,
        Some(json!({
            "lease_token": restore_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete restored spawn: {}",
        done.body
    );

    let after_restore = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "allow_host": false })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH allow_host=false: {}",
        patched.body
    );
    assert_eq!(patched.body["allow_host"], false, "{}", patched.body);
    assert_eq!(patched.body["status"], "online", "{}", patched.body);
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH allow_host=false /me/lease");

    let paused = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "allow_host": true, "paused": true })),
    )
    .await;
    assert_eq!(
        paused.status,
        StatusCode::OK,
        "PATCH paused: {}",
        paused.body
    );
    assert_eq!(paused.body["allow_host"], true, "{}", paused.body);
    assert_eq!(paused.body["paused"], true, "{}", paused.body);
    assert_eq!(paused.body["status"], "online", "{}", paused.body);
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH paused /me/lease");

    let leftover = fleet_db::repos::work_claims::get(&state.db, after_restore)
        .await
        .expect("get leftover claim")
        .unwrap_or_else(|| panic!("leftover claim {after_restore} missing"));
    assert_eq!(
        leftover.status, "ready",
        "allow_host/paused gates must not consume the queued spawn: {leftover:?}"
    );
}

/// catalog:list-paused-host-lease-null — UI Stop taking jobs PATCHes
/// `paused=true`. `host_accepts_leases` is enabled && !paused && !drain &&
/// online. An enqueued spawn_session for that hid must still yield
/// `{claim: null}` after pause, drain, or enabled=false. Online + accepting
/// still leases. Distinct from list-holding-new-jobs-skips-assignable
/// (picker / list_assignable) and list-revoke-host-not-assignable (revoked
/// row). Member PATCH paused stays 403. Product failure: a held box still
/// starts the job.
#[tokio::test]
async fn paused_host_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "paused-lease-box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let before = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "GET after pair+heartbeat: {}",
        before.body
    );
    assert_eq!(
        before.body["id"].as_str(),
        Some(hid.as_str()),
        "{}",
        before.body
    );
    assert_eq!(before.body["status"], "online", "{}", before.body);
    assert_eq!(before.body["paused"], false, "{}", before.body);
    assert_eq!(before.body["drain"], false, "{}", before.body);
    assert_eq!(before.body["enabled"], true, "{}", before.body);
    assert_eq!(before.body["allow_host"], true, "{}", before.body);

    let first = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let first_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        first_lease.status,
        StatusCode::OK,
        "online accepting host must lease: {}",
        first_lease.body
    );
    let first_claim = first_lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "pair+heartbeat host must claim an enqueued spawn_session (not a tautology that every lease is null): {}",
            first_lease.body
        )
    });
    assert_eq!(
        first_claim["id"].as_str(),
        Some(first.to_string().as_str()),
        "first claim.id: {}",
        first_lease.body
    );
    assert_eq!(first_claim["payload"]["kind"], "spawn_session");
    assert_eq!(
        first_claim["payload"]["host_id"].as_str(),
        Some(hid.as_str()),
        "first claim host_id: {}",
        first_lease.body
    );
    let first_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("first lease_token: {}", first_lease.body))
        .to_string();
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{first}/complete"),
        &machine,
        Some(json!({
            "lease_token": first_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete first spawn: {}",
        done.body
    );

    let leftover = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    assert_claim_unleased(&state, leftover, "fresh enqueue before holds").await;

    for (label, patch, field, want) in [
        (
            "paused=true",
            json!({ "paused": true }),
            "paused",
            json!(true),
        ),
        ("drain=true", json!({ "drain": true }), "drain", json!(true)),
        (
            "enabled=false",
            json!({ "enabled": false }),
            "enabled",
            json!(false),
        ),
    ] {
        let reset = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({
                "paused": false,
                "drain": false,
                "enabled": true
            })),
        )
        .await;
        assert_eq!(
            reset.status,
            StatusCode::OK,
            "reset before {label}: {}",
            reset.body
        );
        assert_eq!(reset.body["paused"], false, "reset paused: {}", reset.body);
        assert_eq!(reset.body["drain"], false, "reset drain: {}", reset.body);
        assert_eq!(reset.body["enabled"], true, "reset enabled: {}", reset.body);

        let patched = call(&app, "PATCH", &host_path, Some(&jar), Some(patch)).await;
        assert_eq!(
            patched.status,
            StatusCode::OK,
            "owner PATCH {label} must be 200: {}",
            patched.body
        );
        assert_eq!(
            patched.body["id"].as_str(),
            Some(hid.as_str()),
            "PATCH {label} hid: {}",
            patched.body
        );
        assert_eq!(
            patched.body[field], want,
            "owner PATCH {label} must persist: {}",
            patched.body
        );
        assert!(
            patched.body.get("pair_token").is_none()
                || patched.body["pair_token"].is_null()
                || patched.body["pair_token"]
                    .as_str()
                    .is_none_or(str::is_empty),
            "PATCH {label} must not include pair_token: {}",
            patched.body
        );
        assert!(
            patched.body.get("machine_token").is_none()
                || patched.body["machine_token"].is_null()
                || patched.body["machine_token"]
                    .as_str()
                    .is_none_or(str::is_empty),
            "PATCH {label} must not include machine_token: {}",
            patched.body
        );
        if field != "enabled" {
            assert_eq!(
                patched.body["status"], "online",
                "PATCH {label} must stay online (Stop taking jobs, not revoke): {}",
                patched.body
            );
        }
        if field != "paused" {
            assert_eq!(
                patched.body["paused"], false,
                "PATCH {label} must not also write paused: {}",
                patched.body
            );
        }
        if field != "drain" {
            assert_eq!(
                patched.body["drain"], false,
                "PATCH {label} must not also write drain: {}",
                patched.body
            );
        }
        if field != "enabled" {
            assert_eq!(
                patched.body["enabled"], true,
                "PATCH {label} must not also write enabled=false: {}",
                patched.body
            );
        }
        assert_eq!(
            patched.body["allow_host"], true,
            "this lock is paused/drain/enabled, not allow_host: {}",
            patched.body
        );

        let got = call(&app, "GET", &host_path, Some(&jar), None).await;
        assert_eq!(
            got.status,
            StatusCode::OK,
            "GET after PATCH {label}: {}",
            got.body
        );
        assert_eq!(
            got.body[field], want,
            "GET after PATCH {label} must persist: {}",
            got.body
        );

        let lease = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/lease",
            &machine,
            Some(json!({})),
        )
        .await;
        assert_lease_claim_null(&lease, &format!("PATCH {label} /me/lease"));
        assert_claim_unleased(
            &state,
            leftover,
            &format!("PATCH {label} must not consume the queued spawn_session"),
        )
        .await;

        let later = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/lease",
            &machine,
            Some(json!({})),
        )
        .await;
        assert_lease_claim_null(&later, &format!("later /me/lease poll after {label}"));
        assert_claim_unleased(
            &state,
            leftover,
            &format!("later poll after {label} must still leave spawn ready"),
        )
        .await;
    }

    let accepting = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "paused": false,
            "drain": false,
            "enabled": true
        })),
    )
    .await;
    assert_eq!(
        accepting.status,
        StatusCode::OK,
        "reset to accepting: {}",
        accepting.body
    );
    assert_eq!(accepting.body["paused"], false, "{}", accepting.body);
    assert_eq!(accepting.body["drain"], false, "{}", accepting.body);
    assert_eq!(accepting.body["enabled"], true, "{}", accepting.body);
    assert_eq!(accepting.body["status"], "online", "{}", accepting.body);

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "member me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let member_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&member_jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        member_patch.status,
        StatusCode::FORBIDDEN,
        "member PATCH paused must stay 403: {}",
        member_patch.body
    );
    assert!(
        member_patch.body.get("paused").is_none() || member_patch.body["paused"] != true,
        "member 403 must not write paused=true: {}",
        member_patch.body
    );

    let after_member = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_member.status,
        StatusCode::OK,
        "GET after member PATCH paused: {}",
        after_member.body
    );
    assert_eq!(
        after_member.body["paused"], false,
        "member PATCH paused must not persist: {}",
        after_member.body
    );
    assert_eq!(after_member.body["drain"], false, "{}", after_member.body);
    assert_eq!(after_member.body["enabled"], true, "{}", after_member.body);
    assert_eq!(
        after_member.body["status"], "online",
        "{}",
        after_member.body
    );
    assert_claim_unleased(
        &state,
        leftover,
        "member 403 must not consume the queued spawn",
    )
    .await;

    let control = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "control /me/lease after un-hold: {}",
        control.body
    );
    let control_claim = control.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "paused=false, drain=false, enabled=true must still lease (not a tautology that every lease is null): {}",
            control.body
        )
    });
    assert_eq!(
        control_claim["id"].as_str(),
        Some(leftover.to_string().as_str()),
        "control must hand out the still-queued spawn: {}",
        control.body
    );
    let control_token = control_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", control.body))
        .to_string();
    let control_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{leftover}/complete"),
        &machine,
        Some(json!({
            "lease_token": control_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        control_done.status,
        StatusCode::OK,
        "complete control claim: {}",
        control_done.body
    );
}

/// catalog:list-paused-host-completes-claimed-lease — Stop taking jobs
/// (PATCH paused/drain/enabled=false) must not 409 a lease already held.
/// `/me/lease` stays `{claim: null}` (list-paused-host-lease-null); heartbeat
/// stays 200 (liveness). `http_complete` is require_machine + complete_owned
/// only — wiring it through `host_accepts_leases` would leave spawn_session
/// / chat_turn leased until the 2-minute reclaim. Distinct from
/// list-expired-host-lease-reclaimable, list-lease-complete-rejects-cookie-jwt,
/// list-lease-complete-garbage-token-keeps-claim, and
/// list-revoke-host-not-assignable (revoke nulls the hash; pause does not).
#[tokio::test]
async fn paused_completes_claimed_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "paused-completes-box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let first = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let first_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        first_lease.status,
        StatusCode::OK,
        "online accepting host must lease: {}",
        first_lease.body
    );
    let first_claim = first_lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "pair+heartbeat host must claim an enqueued spawn_session: {}",
            first_lease.body
        )
    });
    assert_eq!(
        first_claim["id"].as_str(),
        Some(first.to_string().as_str()),
        "first claim.id: {}",
        first_lease.body
    );
    let first_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("first lease_token: {}", first_lease.body))
        .to_string();
    Uuid::parse_str(&first_token)
        .unwrap_or_else(|e| panic!("first lease_token UUID {first_token}: {e}"));
    let claim_id = first;
    assert_claim_still_leased(&state, claim_id, &first_token, "after first /me/lease").await;

    let paused = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        paused.status,
        StatusCode::OK,
        "owner PATCH paused: {}",
        paused.body
    );
    assert_eq!(paused.body["paused"], true, "PATCH paused: {}", paused.body);
    assert_eq!(
        paused.body["status"], "online",
        "paused must stay online (Stop taking jobs, not revoke): {}",
        paused.body
    );
    assert_eq!(paused.body["drain"], false, "{}", paused.body);
    assert_eq!(paused.body["enabled"], true, "{}", paused.body);

    let leftover_while_paused = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let paused_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&paused_lease, "PATCH paused /me/lease");
    assert_claim_unleased(
        &state,
        leftover_while_paused,
        "paused /me/lease must not consume a not-yet-leased spawn (list-paused-host-lease-null)",
    )
    .await;
    assert_claim_still_leased(
        &state,
        claim_id,
        &first_token,
        "paused must not drop the already-held lease",
    )
    .await;

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "paused must not drop liveness /me/heartbeat: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["paused"], true,
        "heartbeat after pause must still report paused: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["status"], "online",
        "heartbeat after pause must stay online: {}",
        hb.body
    );

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": first_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete while paused must be 200 (not host_accepts_leases 409): {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "complete while paused must be ok:true: {}",
        done.body
    );

    let again = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": first_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "second complete must be 409 lease not held or already finished, not 401 invalid machine token: {}",
        again.body
    );
    assert_ne!(
        again.status,
        StatusCode::UNAUTHORIZED,
        "second complete is finished-lease 409, not a revoked flm_: {}",
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or(&again.raw);
    assert!(
        again_err.contains("lease not held") || again_err.contains("already finished"),
        "409 must say lease not held or already finished: {again_err}"
    );
    assert!(
        !again_err.contains("invalid machine token"),
        "paused complete must not look like revoke (list-revoke-host-not-assignable): {again_err}"
    );

    let reset = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "paused": false,
            "drain": false,
            "enabled": true
        })),
    )
    .await;
    assert_eq!(reset.status, StatusCode::OK, "reset: {}", reset.body);
    assert_eq!(reset.body["paused"], false, "{}", reset.body);
    assert_eq!(reset.body["drain"], false, "{}", reset.body);
    assert_eq!(reset.body["enabled"], true, "{}", reset.body);

    // The spawn enqueued while paused is still ready (list-paused-host-lease-null).
    // Lease it as the drain oneshot — a fresh claim after un-pause.
    let drain_claim = leftover_while_paused;
    let drain_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        drain_lease.status,
        StatusCode::OK,
        "lease before drain: {}",
        drain_lease.body
    );
    let drain_obj = drain_lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "accepting host must lease the paused leftover as a fresh claim: {}",
            drain_lease.body
        )
    });
    assert_eq!(
        drain_obj["id"].as_str(),
        Some(drain_claim.to_string().as_str()),
        "drain claim.id must be the paused leftover: {}",
        drain_lease.body
    );
    let drain_token = drain_obj["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("drain lease_token: {}", drain_lease.body))
        .to_string();

    let drained = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "drain": true })),
    )
    .await;
    assert_eq!(
        drained.status,
        StatusCode::OK,
        "owner PATCH drain: {}",
        drained.body
    );
    assert_eq!(drained.body["drain"], true, "{}", drained.body);
    assert_eq!(drained.body["paused"], false, "{}", drained.body);
    assert_eq!(
        drained.body["status"], "online",
        "drain must stay online: {}",
        drained.body
    );

    let drain_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{drain_claim}/complete"),
        &machine,
        Some(json!({
            "lease_token": drain_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        drain_done.status,
        StatusCode::OK,
        "complete while drain must be 200: {}",
        drain_done.body
    );

    let leftover_while_drain = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let after_drain = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&after_drain, "further /me/lease after drain complete");
    assert_claim_unleased(
        &state,
        leftover_while_drain,
        "drain must not hand out a new spawn after complete",
    )
    .await;

    let accepting = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "paused": false,
            "drain": false,
            "enabled": true
        })),
    )
    .await;
    assert_eq!(
        accepting.status,
        StatusCode::OK,
        "reset to accepting: {}",
        accepting.body
    );
    assert_eq!(accepting.body["paused"], false, "{}", accepting.body);
    assert_eq!(accepting.body["drain"], false, "{}", accepting.body);
    assert_eq!(accepting.body["enabled"], true, "{}", accepting.body);
    assert_eq!(accepting.body["status"], "online", "{}", accepting.body);

    // Drain leftover is still ready; leasing + completing it is the
    // accepting control (not a tautology that every complete 409s).
    let control = leftover_while_drain;
    let control_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        control_lease.status,
        StatusCode::OK,
        "control /me/lease while online + not paused: {}",
        control_lease.body
    );
    let control_obj = control_lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "online + not paused must still lease (not a tautology that every complete 409s): {}",
            control_lease.body
        )
    });
    assert_eq!(
        control_obj["id"].as_str(),
        Some(control.to_string().as_str()),
        "control claim.id: {}",
        control_lease.body
    );
    let control_token = control_obj["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("control lease_token: {}", control_lease.body))
        .to_string();
    let control_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{control}/complete"),
        &machine,
        Some(json!({
            "lease_token": control_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        control_done.status,
        StatusCode::OK,
        "control complete while accepting: {}",
        control_done.body
    );

    let cookie_claim = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let cookie_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        cookie_lease.status,
        StatusCode::OK,
        "lease before cookie-only complete: {}",
        cookie_lease.body
    );
    let cookie_obj = cookie_lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "accepting host must lease before cookie-only complete: {}",
            cookie_lease.body
        )
    });
    assert_eq!(
        cookie_obj["id"].as_str(),
        Some(cookie_claim.to_string().as_str()),
        "cookie claim.id: {}",
        cookie_lease.body
    );
    let cookie_token = cookie_obj["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("cookie lease_token: {}", cookie_lease.body))
        .to_string();
    let cookie_only = call(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{cookie_claim}/complete"),
        Some(&jar),
        Some(json!({
            "lease_token": cookie_token,
            "ok": true
        })),
    )
    .await;
    assert_cookie_rejected_as_machine(
        &cookie_only,
        "owner Cookie-only /me/lease/{claim_id}/complete (list-lease-complete-rejects-cookie-jwt)",
    );
    assert_claim_still_leased(
        &state,
        cookie_claim,
        &cookie_token,
        "cookie-only complete must not finish the held lease",
    )
    .await;
    let cookie_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{cookie_claim}/complete"),
        &machine,
        Some(json!({
            "lease_token": cookie_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        cookie_done.status,
        StatusCode::OK,
        "flm_ complete after cookie 401: {}",
        cookie_done.body
    );

    let revoked = call(&app, "DELETE", &host_path, Some(&jar), None).await;
    assert_eq!(
        revoked.status,
        StatusCode::OK,
        "owner DELETE revoke: {}",
        revoked.body
    );
    assert_eq!(
        revoked.body["status"], "disabled",
        "revoke persists disabled: {}",
        revoked.body
    );
    let revoked_hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(
        revoked_hb.status,
        StatusCode::UNAUTHORIZED,
        "revoked flm_ /me/heartbeat must stay 401 (list-revoke-host-not-assignable): {}",
        revoked_hb.body
    );
    let revoked_err = revoked_hb.body["error"].as_str().unwrap_or(&revoked_hb.raw);
    assert_eq!(
        revoked_err, "invalid machine token",
        "revoke nulls the hash — heartbeat is 401 invalid machine token, not pause-style 200: {revoked_err:?}: {}",
        revoked_hb.body
    );
}

/// Owner PATCH `allow_host=false` is the plane restriction on host jobs.
/// Distinct from `paused` / `drain` / `enabled=false` (list-paused-host-lease-null)
/// and from advertised_host=false (list-advertised-host-false-lease-null).
/// The box stays online; `/me/lease` must be `{claim: null}` and a later tick
/// must not assign a new session.
#[tokio::test]
async fn allow_host_false_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "allow-host-off", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let first = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "first lease: {}", lease.body);
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "online advertised_host host must claim spawn_session (enqueue first if the queue is empty): {}",
            lease.body
        )
    });
    assert_eq!(claim["id"].as_str(), Some(first.to_string().as_str()));
    let first_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", lease.body))
        .to_string();
    assert_eq!(claim["payload"]["kind"], "spawn_session");
    assert_eq!(
        claim["payload"]["host_id"].as_str(),
        Some(hid.as_str()),
        "snapshot hid+claim: {}",
        lease.body
    );

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{first}/complete"),
        &machine,
        Some(json!({
            "lease_token": first_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "complete first: {}", done.body);

    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let before = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "get before allow_host flip: {}",
        before.body
    );
    assert_eq!(before.body["allow_host"], true, "{}", before.body);
    assert_eq!(before.body["advertised_host"], true, "{}", before.body);
    assert_eq!(before.body["paused"], false, "{}", before.body);
    assert_eq!(before.body["drain"], false, "{}", before.body);
    assert_eq!(before.body["enabled"], true, "{}", before.body);
    assert_eq!(before.body["status"], "online", "{}", before.body);

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "allow_host": false })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH allow_host=false: {}",
        patched.body
    );
    assert_eq!(patched.body["allow_host"], false, "{}", patched.body);
    assert_eq!(
        patched.body["id"].as_str(),
        Some(hid.as_str()),
        "snapshot hid: {}",
        patched.body
    );
    assert_eq!(
        patched.body["status"], "online",
        "held-host-jobs box must stay online: {}",
        patched.body
    );
    assert_eq!(
        patched.body["paused"], false,
        "this lock is allow_host, not paused: {}",
        patched.body
    );
    assert_eq!(
        patched.body["drain"], false,
        "this lock is allow_host, not drain: {}",
        patched.body
    );
    assert_eq!(
        patched.body["enabled"], true,
        "this lock is allow_host, not enabled=false: {}",
        patched.body
    );
    assert_eq!(
        patched.body["advertised_host"], true,
        "advertised_host must stay true (list-advertised-host-false-lease-null owns that flip): {}",
        patched.body
    );

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "get after allow_host=false: {}",
        got.body
    );
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()), "{}", got.body);
    assert_eq!(got.body["allow_host"], false, "{}", got.body);
    assert_eq!(got.body["status"], "online", "{}", got.body);
    assert_eq!(got.body["paused"], false, "{}", got.body);
    assert_eq!(got.body["drain"], false, "{}", got.body);
    assert_eq!(got.body["enabled"], true, "{}", got.body);
    assert_eq!(got.body["advertised_host"], true, "{}", got.body);
    let raw = got.body["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("last_seen_at after allow_host=false: {}", got.body));
    let seen = chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("last_seen_at parse {raw}: {e}"))
        .with_timezone(&chrono::Utc);
    let age = chrono::Utc::now().signed_duration_since(seen);
    assert!(
        age < chrono::Duration::seconds(30) && age > chrono::Duration::seconds(-5),
        "host must stay online with recent last_seen ({age}): {}",
        got.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH allow_host=false /me/lease");
    assert_claim_unleased(
        &state,
        queued,
        "allow_host=false must not consume the queued spawn_session",
    )
    .await;
    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "after PATCH allow_host=false",
    )
    .await;

    let later = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&later, "later /me/lease poll after allow_host=false");
    assert_claim_unleased(
        &state,
        queued,
        "later poll after allow_host=false must still leave spawn ready",
    )
    .await;

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "member me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;

    let member_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&member_jar),
        Some(json!({ "allow_host": true })),
    )
    .await;
    assert_eq!(
        member_patch.status,
        StatusCode::FORBIDDEN,
        "member PATCH allow_host must stay 403: {}",
        member_patch.body
    );
    assert!(
        member_patch.body.get("allow_host").is_none() || member_patch.body["allow_host"] != true,
        "member 403 must not write allow_host=true: {}",
        member_patch.body
    );

    let other_ws_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&other_jar),
        Some(json!({ "allow_host": true })),
    )
    .await;
    assert_eq!(
        other_ws_patch.status,
        StatusCode::FORBIDDEN,
        "other-ws PATCH allow_host must stay 403: {}",
        other_ws_patch.body
    );
    assert!(
        other_ws_patch.body.get("allow_host").is_none()
            || other_ws_patch.body["allow_host"] != true,
        "other-ws 403 must not write allow_host=true: {}",
        other_ws_patch.body
    );

    let other_ws_own = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(json!({ "allow_host": true })),
    )
    .await;
    assert_ne!(
        other_ws_own.status,
        StatusCode::OK,
        "other workspace path must not flip this hid allow_host: {}",
        other_ws_own.body
    );
    assert_ne!(
        other_ws_own.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "other workspace PATCH allow_host must not 5xx: {}",
        other_ws_own.body
    );

    let after_denied = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_denied.status,
        StatusCode::OK,
        "GET after denied PATCH: {}",
        after_denied.body
    );
    assert_eq!(
        after_denied.body["allow_host"], false,
        "member/other-ws must not write allow_host: {}",
        after_denied.body
    );
    assert_eq!(
        after_denied.body["status"], "online",
        "denied PATCH must not take the host offline: {}",
        after_denied.body
    );
    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after denied PATCH"));
    assert!(
        !stored.allow_host,
        "member/other-ws must not persist allow_host=true: {stored:?}"
    );
    assert!(
        stored.enabled && !stored.paused && !stored.drain,
        "denied PATCH must not flip paused/drain/enabled: {stored:?}"
    );

    let restored = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "allow_host": true })),
    )
    .await;
    assert_eq!(
        restored.status,
        StatusCode::OK,
        "owner PATCH allow_host=true: {}",
        restored.body
    );
    assert_eq!(restored.body["allow_host"], true, "{}", restored.body);
    assert_eq!(
        restored.body["paused"], false,
        "restore is allow_host=true, not paused=false: {}",
        restored.body
    );
    assert_eq!(restored.body["status"], "online", "{}", restored.body);

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "restored allow_host lease: {}",
        lease.body
    );
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "allow_host=true must be able to claim again (not a tautology that every lease is null): {}",
            lease.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(queued.to_string().as_str()),
        "restored host must claim the still-queued spawn: {}",
        lease.body
    );
    assert_eq!(claim["payload"]["kind"], "spawn_session");
    assert!(
        claim.get("lease_token").and_then(|t| t.as_str()).is_some(),
        "restored claim must include lease_token: {}",
        lease.body
    );
}

/// Heartbeat `current_jobs` at effective max makes `/me/lease` `{claim: null}`.
/// `list_assignable` already skips `at_job_cap`; lease must too (not
/// client-only stop-claim). Advertised `0` is a cap of 1, not unlimited.
#[tokio::test]
async fn at_job_cap_lease_null_does_not_oversubscribe() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "at-cap", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_concurrent": 1 })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH max_concurrent=1: {}",
        patched.body
    );
    assert_eq!(patched.body["max_concurrent"], 1, "{}", patched.body);
    assert_eq!(patched.body["status"], "online", "{}", patched.body);

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 1,
            "max_concurrent": 8
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat current_jobs=1: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["current_jobs"], 1, "{}", hb.body);
    assert_eq!(
        hb.body["host"]["max_concurrent"], 1,
        "plane 1 must win over advertised 8: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["advertised_max_concurrent"], 8,
        "{}",
        hb.body
    );
    assert_eq!(hb.body["host"]["status"], "online", "{}", hb.body);

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after at-cap heartbeat: {}",
        got.body
    );
    assert_eq!(got.body["current_jobs"], 1, "{}", got.body);
    assert_eq!(got.body["max_concurrent"], 1, "{}", got.body);
    assert_eq!(got.body["advertised_max_concurrent"], 8, "{}", got.body);
    assert_host_still_online_accepting(&got.body, &hid, "after current_jobs=1 at plane cap 1");

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "current_jobs at effective max /me/lease");
    assert_claim_unleased(
        &state,
        claim_id,
        "at-cap host must not consume the queued spawn",
    )
    .await;

    let assignable = worker_hosts_assignable(&state, &ws).await;
    assert!(
        !assignable.contains(&host_id),
        "list_assignable must omit an at-cap hid (do not weaken list-jobs-n-cap-min-advertised): {assignable:?}"
    );
    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "at plane cap 1 with current_jobs=1 (do not weaken list-pick-at-cap-selects-other-host)",
    )
    .await;

    // Advertised 0 is a cap of 1, not unlimited — even if plane is 4.
    let reset = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_concurrent": 4 })),
    )
    .await;
    assert_eq!(
        reset.status,
        StatusCode::OK,
        "reset plane max to 4: {}",
        reset.body
    );
    assert_eq!(reset.body["max_concurrent"], 4, "{}", reset.body);

    let hb0 = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 1,
            "max_concurrent": 0
        })),
    )
    .await;
    assert_eq!(
        hb0.status,
        StatusCode::OK,
        "heartbeat advertised 0: {}",
        hb0.body
    );
    assert!(
        hb0.body["host"]["advertised_max_concurrent"].is_number(),
        "advertised max_concurrent=0 must be stored (not unlimited / omitted): {}",
        hb0.body
    );
    assert_eq!(
        hb0.body["host"]["max_concurrent"], 4,
        "plane stays 4: {}",
        hb0.body
    );
    assert_eq!(hb0.body["host"]["current_jobs"], 1, "{}", hb0.body);

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after advertised 0: {}",
        got.body
    );
    assert_eq!(got.body["current_jobs"], 1, "{}", got.body);
    assert_eq!(got.body["max_concurrent"], 4, "{}", got.body);
    assert!(
        got.body["advertised_max_concurrent"].is_number(),
        "GET must persist advertised_max_concurrent after 0 report: {}",
        got.body
    );
    assert_host_still_online_accepting(&got.body, &hid, "after advertised 0 + current_jobs=1");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &lease,
        "advertised max_concurrent=0 + current_jobs=1 is at cap (0 is not unlimited)",
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "advertised-0 at-cap must not consume the queued spawn",
    )
    .await;
    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "after advertised 0 + current_jobs=1",
    )
    .await;

    // Control: under cap, the same queued spawn is claimable.
    let hb_free = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 8
        })),
    )
    .await;
    assert_eq!(
        hb_free.status,
        StatusCode::OK,
        "heartbeat current_jobs=0: {}",
        hb_free.body
    );
    assert_eq!(hb_free.body["host"]["current_jobs"], 0, "{}", hb_free.body);

    let cookie_lease = call(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        cookie_lease.status,
        StatusCode::UNAUTHORIZED,
        "cookie-only /me/lease must stay 401: {}",
        cookie_lease.body
    );
    assert!(
        cookie_lease.body.get("claim").is_none() || cookie_lease.body["claim"].is_null(),
        "cookie-only lease must not return a claim: {}",
        cookie_lease.body
    );
    assert!(
        !cookie_lease.body.to_string().contains("lease_token"),
        "cookie-only lease must not return a lease_token: {}",
        cookie_lease.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "under-cap lease: {}",
        lease.body
    );
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "current_jobs=0 must claim the still-queued spawn (not a tautology that every lease is null): {}",
            lease.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "under-cap host must claim the still-queued spawn: {}",
        lease.body
    );
    let token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token under cap: {}", lease.body))
        .to_string();

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete under-cap spawn: {}",
        done.body
    );

    let leftover = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let paused = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        paused.status,
        StatusCode::OK,
        "PATCH paused: {}",
        paused.body
    );
    assert_eq!(paused.body["paused"], true, "{}", paused.body);
    assert_eq!(paused.body["status"], "online", "{}", paused.body);
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH paused /me/lease");
    assert_claim_unleased(&state, leftover, "paused must not consume the queued spawn").await;
}

#[tokio::test]
async fn lease_null_when_both_effective_modes_false() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "idle", false, false).await;
    assert_eq!(
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            &machine,
            Some(json!({
                "advertised_host": false,
                "advertised_container": false,
                "current_jobs": 0
            })),
        )
        .await
        .body["effective"],
        json!({ "host": false, "container": false, "max_concurrent": 4 })
    );

    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert!(
        lease.body["claim"].is_null(),
        "both effective modes false must not lease: {}",
        lease.body
    );
}

#[tokio::test]
async fn lease_null_when_only_container_is_advertised() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "k3d-only", false, true).await;
    assert_eq!(
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            &machine,
            Some(json!({
                "advertised_host": false,
                "advertised_container": true,
                "current_jobs": 0
            })),
        )
        .await
        .body["effective"]["host"],
        false
    );

    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert!(
        lease.body["claim"].is_null(),
        "v1 must not lease to a container-only host: {}",
        lease.body
    );
}

#[tokio::test]
async fn lease_only_returns_claim_for_this_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_a, machine_a) =
        create_pair_and_heartbeat(&app, &jar, &ws, "box-a", true, false).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_a = enqueue_spawn_for_host(&state, workspace_id, host_a).await;
    let claim_b = enqueue_spawn_for_host(&state, workspace_id, Uuid::new_v4()).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine_a,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let claim = lease.body["claim"].as_object().expect("claim object");
    assert_eq!(claim["id"].as_str(), Some(claim_a.to_string().as_str()));
    assert_ne!(claim["id"].as_str(), Some(claim_b.to_string().as_str()));

    let again = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine_a,
        Some(json!({})),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "second lease: {}", again.body);
    assert!(
        again.body["claim"].is_null(),
        "must not lease another host's claim: {}",
        again.body
    );
}

#[tokio::test]
async fn complete_rejects_other_host_same_lease_token() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_a, machine_a) =
        create_pair_and_heartbeat(&app, &jar, &ws, "box-a", true, false).await;
    let (_host_b, machine_b) =
        create_pair_and_heartbeat(&app, &jar, &ws, "box-b", true, false).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_a).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine_a,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let lease_token = lease.body["claim"]["lease_token"]
        .as_str()
        .expect("lease_token")
        .to_string();
    assert_eq!(
        lease.body["claim"]["id"].as_str(),
        Some(claim_id.to_string().as_str())
    );

    let stolen = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine_b,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::CONFLICT,
        "other host complete: {}",
        stolen.body
    );

    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine_a,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "owner complete: {}", done.body);
}

/// Browser session cookies (`fleet_access` JWT, `fleet_refresh` opaque)
/// cannot finish a held lease. `POST /me/lease/{claim_id}/complete`
/// requires `Authorization: Bearer flm_…` — a stolen login must not mark
/// a `spawn_session` done (or write a `chat_turn` result).
/// Catalog: list-lease-complete-rejects-cookie-jwt.
/// Distinct from cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease
/// (heartbeat/lease only) and complete_rejects_other_host_same_lease_token
/// (host B's flm_ on host A's claim).
#[tokio::test]
async fn lease_complete_cookie_jwt_cannot_finish_held_claim() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("signup login must set fleet_refresh")
        .to_string();
    assert!(
        access.contains('.') && access.split('.').count() == 3,
        "fleet_access must be a JWT, got {access:?}"
    );
    assert!(
        !access.starts_with("flm_"),
        "fleet_access JWT must not look like a machine token: {access}"
    );
    assert!(
        !refresh.starts_with("flm_"),
        "fleet_refresh must not look like a machine token: {refresh}"
    );

    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let claim = lease.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("real flm_ must lease the spawn: {}", lease.body));
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "lease claim.id: {}",
        lease.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", lease.body))
        .to_string();
    Uuid::parse_str(&lease_token).unwrap_or_else(|e| panic!("lease_token UUID {lease_token}: {e}"));
    assert_claim_still_leased(&state, claim_id, &lease_token, "after real flm_ lease").await;

    let complete_path = format!("/api/worker-hosts/me/lease/{claim_id}/complete");
    let complete_body = json!({
        "lease_token": lease_token,
        "ok": true
    });

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::OK,
        "owner session must be live before complete probes: {}",
        me.body
    );

    let cookie_only = call(
        &app,
        "POST",
        &complete_path,
        Some(&jar),
        Some(complete_body.clone()),
    )
    .await;
    assert_cookie_rejected_as_machine(
        &cookie_only,
        "owner Cookie-only (no Authorization) /me/lease/{claim_id}/complete",
    );
    assert_ne!(
        cookie_only.body.get("ok"),
        Some(&json!(true)),
        "cookie-only complete must not include ok:true: {}",
        cookie_only.body
    );
    assert_claim_still_leased(&state, claim_id, &lease_token, "after cookie-only complete").await;

    for (token, label) in [
        (access.as_str(), "Bearer <fleet_access JWT>"),
        (refresh.as_str(), "Bearer <fleet_refresh opaque>"),
    ] {
        let jwt_only = call_bearer(
            &app,
            "POST",
            &complete_path,
            token,
            Some(complete_body.clone()),
        )
        .await;
        assert_user_jwt_rejected_as_machine(
            &jwt_only,
            &format!("{label} /me/lease/{{claim_id}}/complete"),
        );
        assert_ne!(
            jwt_only.body.get("ok"),
            Some(&json!(true)),
            "{label} complete must not include ok:true: {}",
            jwt_only.body
        );
        assert_claim_still_leased(
            &state,
            claim_id,
            &lease_token,
            &format!("after {label} complete"),
        )
        .await;

        let cookie_plus = call_inner(
            &app,
            "POST",
            &complete_path,
            Some(&jar),
            Some(token),
            Some(complete_body.clone()),
        )
        .await;
        assert_user_jwt_rejected_as_machine(
            &cookie_plus,
            &format!("owner Cookie + {label} /me/lease/{{claim_id}}/complete"),
        );
        assert_ne!(
            cookie_plus.body.get("ok"),
            Some(&json!(true)),
            "Cookie + {label} complete must not include ok:true: {}",
            cookie_plus.body
        );
        assert_claim_still_leased(
            &state,
            claim_id,
            &lease_token,
            &format!("after Cookie + {label} complete"),
        )
        .await;
    }

    let cookie_plus_flm = call_inner(
        &app,
        "POST",
        &complete_path,
        Some(&jar),
        Some(machine.as_str()),
        Some(complete_body.clone()),
    )
    .await;
    assert_eq!(
        cookie_plus_flm.status,
        StatusCode::OK,
        "Cookie + Bearer flm_ complete: {}",
        cookie_plus_flm.body
    );
    assert_eq!(
        cookie_plus_flm.body.get("ok"),
        Some(&json!(true)),
        "Cookie + Bearer flm_ complete must be ok:true (cookie must not replace flm_): {}",
        cookie_plus_flm.body
    );

    let again = call_bearer(&app, "POST", &complete_path, &machine, Some(complete_body)).await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "second complete after Cookie+flm_ must be 409 lease not held: {}",
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or(&again.raw);
    assert!(
        again_err.contains("lease not held"),
        "second complete must say lease not held (same as complete_rejects_other_host): {again_err}"
    );
    assert_ne!(
        again.body.get("ok"),
        Some(&json!(true)),
        "second complete must not include ok:true: {}",
        again.body
    );

    let me_after = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me_after.status,
        StatusCode::OK,
        "owner session must still be live after complete probes: {}",
        me_after.body
    );
}

/// Garbage / missing `lease_token` must 400 (or 422 if the key is omitted)
/// before `complete_owned`. A wildcard or complete-then-400 would finish the
/// spawn without the lease secret; the real `flm_` later 409s.
#[tokio::test]
async fn lease_complete_garbage_token_keeps_claim() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_a, machine_a) =
        create_pair_and_heartbeat(&app, &jar, &ws, "box-a", true, false).await;
    let (_host_b, machine_b) =
        create_pair_and_heartbeat(&app, &jar, &ws, "box-b", true, false).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_a).await;

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine_a,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let claim = lease.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("lease must return a claim: {}", lease.body));
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "lease claim.id: {}",
        lease.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", lease.body))
        .to_string();
    Uuid::parse_str(&lease_token).unwrap_or_else(|e| panic!("lease_token UUID {lease_token}: {e}"));

    let complete_path = format!("/api/worker-hosts/me/lease/{claim_id}/complete");
    for garbage in ["", "   ", "not-a-uuid", "0"] {
        let out = call_bearer(
            &app,
            "POST",
            &complete_path,
            &machine_a,
            Some(json!({
                "lease_token": garbage,
                "ok": true
            })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "garbage {garbage:?} must not 200 complete: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::CONFLICT,
            "garbage {garbage:?} must not 409 already finished: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "garbage {garbage:?} must be 400 lease_token must be a UUID, got {}: {}",
            out.status,
            out.body
        );
        let err = out.body["error"].as_str().unwrap_or(&out.raw);
        assert!(
            err.contains("lease_token must be a UUID"),
            "garbage {garbage:?} must say lease_token must be a UUID, got {err:?}: {}",
            out.body
        );
        assert_ne!(
            out.body.get("ok"),
            Some(&json!(true)),
            "garbage {garbage:?} must not include ok:true: {}",
            out.body
        );
        assert_claim_still_leased(
            &state,
            claim_id,
            &lease_token,
            &format!("after garbage lease_token {garbage:?}"),
        )
        .await;
    }

    let omitted = call_bearer(
        &app,
        "POST",
        &complete_path,
        &machine_a,
        Some(json!({ "ok": true })),
    )
    .await;
    assert!(
        omitted.status == StatusCode::UNPROCESSABLE_ENTITY
            || omitted.status == StatusCode::BAD_REQUEST,
        "omitted lease_token must be 422 missing field or 400, got {}: {}",
        omitted.status,
        omitted.body
    );
    assert_ne!(
        omitted.status,
        StatusCode::OK,
        "omitted lease_token must not 200 complete: {}",
        omitted.body
    );
    assert_ne!(
        omitted.status,
        StatusCode::CONFLICT,
        "omitted lease_token must not 409 already finished: {}",
        omitted.body
    );
    assert_ne!(
        omitted.body.get("ok"),
        Some(&json!(true)),
        "omitted lease_token must not include ok:true: {}",
        omitted.body
    );
    assert_claim_still_leased(&state, claim_id, &lease_token, "after omitted lease_token").await;

    let cookie_only = call(
        &app,
        "POST",
        &complete_path,
        Some(&jar),
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_cookie_rejected_as_machine(&cookie_only, "Cookie-only /me/lease/{claim_id}/complete");
    assert_claim_still_leased(&state, claim_id, &lease_token, "after cookie-only complete").await;

    let stolen = call_bearer(
        &app,
        "POST",
        &complete_path,
        &machine_b,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::CONFLICT,
        "other host complete: {}",
        stolen.body
    );
    assert_ne!(
        stolen.body.get("ok"),
        Some(&json!(true)),
        "other host complete must not include ok:true: {}",
        stolen.body
    );
    assert_claim_still_leased(&state, claim_id, &lease_token, "after other-host complete").await;

    let done = call_bearer(
        &app,
        "POST",
        &complete_path,
        &machine_a,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "owner complete after garbage attempts: {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "owner complete must be ok:true: {}",
        done.body
    );

    let again = call_bearer(
        &app,
        "POST",
        &complete_path,
        &machine_a,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "second complete: {}",
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or(&again.raw);
    assert!(
        again_err.contains("lease not held"),
        "second complete must say lease not held: {again_err}"
    );
}

#[tokio::test]
async fn install_worker_sh_starts_with_shebang() {
    let app = app().await;
    let out = call(&app, "GET", "/install-worker.sh", None, None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert!(
        out.raw.starts_with("#!"),
        "install-worker.sh should start with shebang: {}",
        &out.raw[..out.raw.len().min(80)]
    );
}

#[tokio::test]
async fn install_linux_x86_64_script_ok() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/linux-x86_64.sh", None, None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert!(
        out.raw.contains("linux-x86_64") || out.raw.starts_with("#!"),
        "{}",
        &out.raw[..out.raw.len().min(120)]
    );
}

#[tokio::test]
async fn install_path_traversal_rejected() {
    let app = app().await;
    let out = call(
        &app,
        "GET",
        "/install/worker/../../../etc/passwd",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
}

/// Unset FLEET_WORKER_DIST_DIR: scripts still 200; missing bin is 404 with a build hint.
#[tokio::test]
async fn unset_worker_dist_scripts_serve_and_bin_get_404_with_build_hint() {
    let (app, _) = app_with_state_dist(None).await;

    let sh = call(&app, "GET", "/install-worker.sh", None, None).await;
    assert_eq!(sh.status, StatusCode::OK, "{}", sh.raw);
    assert!(
        !sh.raw.is_empty(),
        "install-worker.sh must still serve a body"
    );

    let ps1 = call(&app, "GET", "/install-worker.ps1", None, None).await;
    assert_eq!(ps1.status, StatusCode::OK, "{}", ps1.raw);
    assert!(
        !ps1.raw.is_empty(),
        "install-worker.ps1 must still serve a body"
    );

    let out = call(
        &app,
        "GET",
        "/install/worker/bin/no-such-os/fleet-host",
        None,
        None,
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "unset dist must 404 the missing bin, not 500 or a silent empty: {}",
        out.raw
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("binary not available"),
        "404 must say the binary is missing: {err}"
    );
    assert!(
        err.contains("FLEET_WORKER_DIST_DIR"),
        "404 must name FLEET_WORKER_DIST_DIR: {err}"
    );
    assert!(
        err.contains("FLEET_WORKER_BUILD_FROM_SOURCE"),
        "404 must name FLEET_WORKER_BUILD_FROM_SOURCE: {err}"
    );
    assert!(
        err.contains("no-such-os/fleet-host"),
        "404 must name the requested platform/bin: {err}"
    );
}

#[tokio::test]
async fn install_worker_bin_from_dist_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let payload = b"fake-fleet-host-from-dist";
    std::fs::write(plat.join("fleet-host"), payload).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/fleet-host",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert_eq!(out.raw.as_bytes(), payload);
    let ct = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "application/octet-stream", "{ct}");
}

#[tokio::test]
async fn install_worker_bin_head_reports_length_without_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let payload = b"fake-fleet-host-from-dist";
    std::fs::write(plat.join("fleet-host"), payload).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "HEAD",
        "/install/worker/bin/linux-x86_64/fleet-host",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert!(
        out.raw.is_empty(),
        "HEAD must not send the file: {}",
        out.raw
    );
    let cl = out
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cl, payload.len().to_string(), "{cl}");
}

#[tokio::test]
async fn install_worker_bin_dotdot_rejected() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/bin/../fleet-host", None, None).await;
    assert!(
        out.status == StatusCode::BAD_REQUEST || out.status == StatusCode::NOT_FOUND,
        "{}",
        out.raw
    );
}

/// Public GET `/install/worker/bin/{platform}/{bin}` is host/driver artifacts
/// only. `fleet-server` / `fleet-ui` / `cargo` / a planted `.env` must not
/// become an unauthenticated download, including via native `target/` fallback
/// or a dist-dir file of an unknown name.
#[tokio::test]
async fn install_bin_allowlist_rejects_control_plane_and_unknown_names() {
    let native = fleet_server::install_routes::native_platform_id();
    assert!(
        !native.is_empty() && !native.contains('/') && !native.contains(".."),
        "native_platform_id must be a single path token: {native}"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let host_payload = b"fake-fleet-host-allowlist-control";
    let forbidden_marker = "PLANTED-CONTROL-PLANE-NOT-A-WORKER";
    let env_marker = "SECRET=planted-control-plane-env";
    let passwd_marker = "root:x:0:0:planted-passwd";
    let host_name = if native.starts_with("windows") {
        "fleet-host.exe"
    } else {
        "fleet-host"
    };

    let plant = |platform: &str, name: &str, body: &[u8]| {
        let plat = dir.path().join(platform);
        std::fs::create_dir_all(&plat).unwrap();
        std::fs::write(plat.join(name), body).unwrap();
    };

    for platform in ["linux-x86_64", native.as_str()] {
        plant(platform, "fleet-host", host_payload);
        plant(platform, "fleet-host.exe", host_payload);
        plant(
            platform,
            "fleet-server",
            format!("{forbidden_marker}-fleet-server").as_bytes(),
        );
        plant(
            platform,
            "fleet-ui",
            format!("{forbidden_marker}-fleet-ui").as_bytes(),
        );
        plant(
            platform,
            "cargo",
            format!("{forbidden_marker}-cargo").as_bytes(),
        );
        plant(
            platform,
            "fleet-server.exe",
            format!("{forbidden_marker}-fleet-server.exe").as_bytes(),
        );
        plant(platform, ".env", env_marker.as_bytes());
        plant(platform, "passwd", passwd_marker.as_bytes());
        plant(platform, "fleet-host.sha256", b"abc123  fleet-host\n");
        plant(
            platform,
            "manifest.json",
            br#"{"git_sha":"allowlist-test","platform":"linux-x86_64","bins":{}}"#,
        );
        plant(platform, "libunwind.dll", b"PLANTED-LIBUNWIND-ALLOWED");
    }

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;

    let assert_rejected = |out: &HttpOut, uri: &str, planted: &str| {
        assert_ne!(
            out.status,
            StatusCode::OK,
            "{uri} must not 200 with artifact bytes: {}",
            out.raw
        );
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "{uri} must 404 (not 200 with ELF/PE, not 5xx): {}",
            out.raw
        );
        assert!(
            !out.status.is_server_error(),
            "{uri} must not 5xx: {}",
            out.raw
        );
        let bytes = out.raw.as_bytes();
        assert!(
            !bytes.starts_with(b"\x7fELF") && !bytes.starts_with(b"MZ"),
            "{uri} body must not be the control-plane binary (ELF/PE): status={} prefix={:?}",
            out.status,
            &bytes[..bytes.len().min(8)]
        );
        assert!(
            !out.raw.contains(planted),
            "{uri} must not open a planted non-worker file: {}",
            out.raw
        );
        let err = out.body["error"].as_str().unwrap_or(&out.raw);
        assert!(
            err.contains("binary not available") || err.contains("FLEET_WORKER_DIST_DIR"),
            "{uri} 404 error may name the missing bin / dist dir: {err}"
        );
    };

    for name in ["fleet-server", "fleet-ui", "cargo", "fleet-server.exe"] {
        let uri = format!("/install/worker/bin/{native}/{name}");
        let out = call(&app, "GET", &uri, None, None).await;
        assert_rejected(&out, &uri, forbidden_marker);
    }

    let env_uri = "/install/worker/bin/linux-x86_64/.env";
    let env_out = call(&app, "GET", env_uri, None, None).await;
    assert_rejected(&env_out, env_uri, env_marker);

    let passwd_uri = format!("/install/worker/bin/{native}/passwd");
    let passwd_out = call(&app, "GET", &passwd_uri, None, None).await;
    assert_rejected(&passwd_out, &passwd_uri, passwd_marker);

    let sh = call(&app, "GET", "/install-worker.sh", None, None).await;
    assert_eq!(
        sh.status,
        StatusCode::OK,
        "install-worker.sh must stay public: {}",
        sh.raw
    );
    assert!(
        !sh.raw.is_empty(),
        "install-worker.sh must still serve a body"
    );

    let host_uri = format!("/install/worker/bin/{native}/{host_name}");
    let host = call(&app, "GET", &host_uri, None, None).await;
    assert_eq!(
        host.status,
        StatusCode::OK,
        "staged {host_name} is an allowed worker artifact: {}",
        host.raw
    );
    assert_eq!(host.raw.as_bytes(), host_payload);

    if native != "linux-x86_64" {
        let linux_host = call(
            &app,
            "GET",
            "/install/worker/bin/linux-x86_64/fleet-host",
            None,
            None,
        )
        .await;
        assert_eq!(
            linux_host.status,
            StatusCode::OK,
            "staged linux-x86_64/fleet-host is an allowed worker artifact: {}",
            linux_host.raw
        );
        assert_eq!(linux_host.raw.as_bytes(), host_payload);
    }

    for name in ["fleet-host.sha256", "manifest.json", "libunwind.dll"] {
        let uri = format!("/install/worker/bin/linux-x86_64/{name}");
        let out = call(&app, "GET", &uri, None, None).await;
        assert!(
            out.status == StatusCode::OK || out.status == StatusCode::NOT_FOUND,
            "{name} is an allowed worker name (200 if staged, else 404), not this reject: {}",
            out.raw
        );
        assert!(
            !out.status.is_server_error(),
            "{name} must not 5xx: {}",
            out.raw
        );
    }

    // Native workspace target/{release,debug}/{bin} fallback must not serve
    // fleet-server either (dist unset so lookup can reach target/).
    let (app_fallback, _) = app_with_state_dist(None).await;
    for name in ["fleet-server", "fleet-ui", "cargo", "fleet-server.exe"] {
        let uri = format!("/install/worker/bin/{native}/{name}");
        let out = call(&app_fallback, "GET", &uri, None, None).await;
        assert_rejected(&out, &uri, forbidden_marker);
        let bytes = out.raw.as_bytes();
        assert!(
            !bytes.starts_with(b"\x7fELF") && !bytes.starts_with(b"MZ"),
            "unset-dist {uri} must not serve target/debug or target/release ELF/PE: {}",
            out.raw
        );
    }

    let sh_fallback = call(&app_fallback, "GET", "/install-worker.sh", None, None).await;
    assert_eq!(
        sh_fallback.status,
        StatusCode::OK,
        "install-worker.sh still 200 when dist is unset: {}",
        sh_fallback.raw
    );
}

fn write_platform_manifest(dir: &std::path::Path, platform: &str, host_sha: &str, git_sha: &str) {
    let plat = dir.join(platform);
    std::fs::create_dir_all(&plat).unwrap();
    let body = serde_json::json!({
        "git_sha": git_sha,
        "platform": platform,
        "bins": {
            "fleet-host": { "sha256": host_sha, "size": 12, "version": "0.0.2" },
            "fleet-driver": { "sha256": "driver-sha", "size": 14, "version": "0.0.2" }
        }
    });
    std::fs::write(
        plat.join("manifest.json"),
        serde_json::to_vec_pretty(&body).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn install_worker_manifest_404_when_unstaged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/manifest.json",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.raw);
}

#[tokio::test]
async fn install_worker_manifest_200_when_staged() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/manifest.json",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert_eq!(out.body["git_sha"], "stage-tree");
    assert_eq!(out.body["platform"], "linux-x86_64");
    assert_eq!(out.body["bins"]["fleet-host"]["sha256"], "aaa111");
    assert_eq!(out.body["bins"]["fleet-host"]["size"], 12);
    let ct = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("json"), "content-type={ct}");
}

#[tokio::test]
async fn heartbeat_includes_desired_when_staged() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "software": {
                "git_sha": "old-tree",
                "bins": {
                    "fleet-host": "bbb222",
                    "fleet-driver": "driver-sha"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    assert_eq!(hb.body["desired"]["git_sha"], "stage-tree", "{}", hb.body);
    assert_eq!(
        hb.body["desired"]["bins"]["fleet-host"]["sha256"], "aaa111",
        "{}",
        hb.body
    );
    assert_eq!(hb.body["host"]["software_status"], "behind", "{}", hb.body);
    assert_eq!(hb.body["host"]["outdated"], true, "{}", hb.body);
    assert_eq!(
        hb.body["host"]["desired"]["git_sha"], "stage-tree",
        "{}",
        hb.body
    );
}

#[tokio::test]
async fn public_host_software_status_current_behind_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let (_id_old, machine_old) =
        create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    let unknown = listed.body["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "old-box")
        .expect("old-box");
    assert_eq!(unknown["software_status"], "unknown", "{}", unknown);
    assert_eq!(unknown["outdated"], false, "{}", unknown);
    assert!(
        unknown.get("software").is_none() || unknown["software"].is_null(),
        "no software report: {unknown}"
    );
    assert_eq!(unknown["desired"]["git_sha"], "stage-tree", "{}", unknown);

    let hb_behind = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine_old,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "software": { "bins": { "fleet-host": "not-the-staged-sha" } }
        })),
    )
    .await;
    assert_eq!(hb_behind.status, StatusCode::OK, "{}", hb_behind.body);
    assert_eq!(hb_behind.body["host"]["software_status"], "behind");
    assert_eq!(hb_behind.body["host"]["outdated"], true);

    let hb_current = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine_old,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "software": {
                "git_sha": "other",
                "bins": {
                    "fleet-host": "aaa111",
                    "fleet-driver": "driver-sha"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb_current.status, StatusCode::OK, "{}", hb_current.body);
    assert_eq!(hb_current.body["host"]["software_status"], "current");
    assert_eq!(hb_current.body["host"]["outdated"], false);

    let got = call(
        &app,
        "GET",
        &format!("{}/{}", hosts_path(&ws), unknown["id"].as_str().unwrap()),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["software_status"], "current");
    assert_eq!(got.body["outdated"], false);
    assert_eq!(got.body["desired"]["platform"], "linux-x86_64");
}

#[tokio::test]
async fn member_can_list_but_cannot_mutate() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&owner_jar),
        Some(json!({ "name": "studio" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let list = call(&app, "GET", &hosts_path(&ws), Some(&member_jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "member list: {}", list.body);
    assert!(
        list.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|h| h["id"].as_str() == Some(hid.as_str())),
        "member must see the host: {}",
        list.body
    );
    assert!(
        list.body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h.get("pair_token").is_none() && h.get("machine_token_hash").is_none()),
        "list must not leak tokens: {}",
        list.body
    );

    let create = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&member_jar),
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(
        create.status,
        StatusCode::FORBIDDEN,
        "member create: {}",
        create.body
    );

    let patch = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&member_jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        patch.status,
        StatusCode::FORBIDDEN,
        "member patch: {}",
        patch.body
    );

    let rotate = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&member_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotate.status,
        StatusCode::FORBIDDEN,
        "member rotate: {}",
        rotate.body
    );

    let del = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(
        del.status,
        StatusCode::FORBIDDEN,
        "member delete: {}",
        del.body
    );
}

/// Admin can GET list/detail like a member, but cookie mutate stays
/// `require_owner`. A gate that became owner|admin (invites `can_manage`)
/// would let an admin mint `flp_`, pause/revoke every box, or flip the plane.
#[tokio::test]
async fn admin_cannot_mutate_worker_hosts() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let owner_name = format!("owner-box-{}", Uuid::new_v4());
    let admin_create_name = format!("admin-create-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&owner_jar),
        Some(json!({ "name": owner_name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "owner create: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let hid_uuid = Uuid::parse_str(&hid).expect("hid");
    let status0 = created.body["host"]["status"]
        .as_str()
        .expect("host.status")
        .to_string();
    assert_eq!(
        status0, "pending",
        "owner create starts pending: {}",
        created.body
    );
    let paused0 = created.body["host"]["paused"]
        .as_bool()
        .expect("host.paused");
    let max0 = created.body["host"]["max_concurrent"]
        .as_i64()
        .expect("host.max_concurrent");

    let admin_email = unique_email();
    let signed = signup(&app, &admin_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "admin signup: {}",
        signed.body
    );
    let (admin_login, admin_jar) = login(&app, &admin_email, TEST_PASSWORD).await;
    assert_eq!(
        admin_login.status,
        StatusCode::OK,
        "admin login: {}",
        admin_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&admin_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "admin me: {}", me.body);
    let admin_id = Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");

    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        admin_id,
        "admin",
        chrono::Utc::now(),
    )
    .await
    .expect("add admin");
    tx.commit().await.expect("commit admin");

    let list = call(&app, "GET", &hosts_path(&ws), Some(&admin_jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "admin list: {}", list.body);
    let list_hosts = list.body["hosts"].as_array().expect("hosts");
    assert!(
        list_hosts
            .iter()
            .any(|h| h["id"].as_str() == Some(hid.as_str())),
        "admin must see the host: {}",
        list.body
    );
    assert!(
        !json_has_key(&list.body, "pair_token") && !json_has_key(&list.body, "machine_token_hash"),
        "admin list must not include pair_token / machine_token_hash: {}",
        list.body
    );
    assert!(
        list_hosts
            .iter()
            .all(|h| h.get("pair_token").is_none() && h.get("machine_token_hash").is_none()),
        "admin list hosts must not leak tokens: {}",
        list.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "admin get: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()), "{}", got.body);
    assert!(
        !json_has_key(&got.body, "pair_token") && !json_has_key(&got.body, "machine_token_hash"),
        "admin get must not include pair_token / machine_token_hash: {}",
        got.body
    );

    let owner_required = |out: &HttpOut, label: &str| {
        assert_eq!(
            out.status,
            StatusCode::FORBIDDEN,
            "{label} must be 403 workspace owner required, not 200 pair/revoke: {}",
            out.body
        );
        let err = out.body["error"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            err.contains("workspace owner required"),
            "{label} must say workspace owner required: {}",
            out.body
        );
        for key in ["pair_token", "machine_token", "install_hint"] {
            assert!(
                !json_has_key(&out.body, key),
                "{label} 403 must not include {key}: {}",
                out.body
            );
        }
    };

    let create = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&admin_jar),
        Some(json!({ "name": admin_create_name })),
    )
    .await;
    owner_required(&create, "admin create");

    let rotate = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&admin_jar),
        Some(json!({})),
    )
    .await;
    owner_required(&rotate, "admin pair-tokens");

    let patch = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&admin_jar),
        Some(json!({ "paused": true, "max_concurrent": 1 })),
    )
    .await;
    owner_required(&patch, "admin patch host");

    let del = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&admin_jar),
        None,
    )
    .await;
    owner_required(&del, "admin delete");

    let owner_list = call(&app, "GET", &hosts_path(&ws), Some(&owner_jar), None).await;
    assert_eq!(
        owner_list.status,
        StatusCode::OK,
        "owner list after admin mutate: {}",
        owner_list.body
    );
    let owner_hosts = owner_list.body["hosts"].as_array().expect("hosts");
    let listed = owner_hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "owner list must still have original hid (not revoked): {}",
                owner_list.body
            )
        });
    assert_eq!(
        listed["status"], status0,
        "still pending: {}",
        owner_list.body
    );
    assert_eq!(
        listed["paused"], paused0,
        "paused unchanged: {}",
        owner_list.body
    );
    assert_eq!(
        listed["max_concurrent"], max0,
        "max_concurrent unchanged: {}",
        owner_list.body
    );
    assert!(
        owner_hosts
            .iter()
            .all(|h| h["name"].as_str() != Some(admin_create_name.as_str())),
        "admin create name must not appear on owner list: {}",
        owner_list.body
    );

    let owner_got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(
        owner_got.status,
        StatusCode::OK,
        "owner get after admin mutate: {}",
        owner_got.body
    );
    assert_eq!(owner_got.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(
        owner_got.body["status"], status0,
        "detail still pending (not revoked): {}",
        owner_got.body
    );
    assert_eq!(
        owner_got.body["paused"], paused0,
        "detail paused unchanged: {}",
        owner_got.body
    );
    assert_eq!(
        owner_got.body["max_concurrent"], max0,
        "detail max_concurrent unchanged: {}",
        owner_got.body
    );

    let rows = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace");
    assert!(
        rows.iter().any(|h| h.id == hid_uuid),
        "original hid must remain a worker_hosts row: {rows:?}"
    );
    assert!(
        rows.iter().all(|h| h.name != admin_create_name),
        "no extra worker_hosts row with the admin create name {admin_create_name}: {rows:?}"
    );

    let minted = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&owner_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        minted.status,
        StatusCode::OK,
        "owner pair-tokens after admin mutate: {}",
        minted.body
    );
    let flp = minted.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("owner pair-tokens must mint flp_: {}", minted.body));
    assert!(flp.starts_with("flp_"), "owner mint prefix: {flp}");

    let owner_pause = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&owner_jar),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        owner_pause.status,
        StatusCode::OK,
        "owner PATCH paused is the control (not every PATCH 403s): {}",
        owner_pause.body
    );
    assert_eq!(
        owner_pause.body["paused"], true,
        "owner pause must stick: {}",
        owner_pause.body
    );
}

#[tokio::test]
async fn new_pair_token_only_while_pending_and_old_token_dies() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "rotate" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let old = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();

    let rotated = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(rotated.status, StatusCode::OK, "rotate: {}", rotated.body);
    let new = rotated.body["pair_token"]
        .as_str()
        .expect("new token")
        .to_string();
    assert_ne!(new, old, "rotate must issue a different token");

    let stale = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": old,
            "hostname": "box",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert!(
        stale.status == StatusCode::UNAUTHORIZED || stale.status == StatusCode::CONFLICT,
        "old token after rotate: {} {}",
        stale.status,
        stale.body
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": new,
            "hostname": "box",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "pair with new: {}",
        paired.body
    );

    let again = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "rotate after pair: {}",
        again.body
    );
}

/// catalog:unit-request-update — Auto-update off + Update now (`request_update`).
#[tokio::test]
async fn request_update_marks_host() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;

    let denied = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        None,
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED, "{}", denied.body);

    let before = call(
        &app,
        "GET",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "get: {}", before.body);
    assert_eq!(
        before.body["auto_update"], true,
        "paired host defaults auto-update on: {}",
        before.body
    );
    assert!(
        before.body.get("update_requested_at").is_none()
            || before.body["update_requested_at"].is_null(),
        "no update requested yet: {}",
        before.body
    );
    let gen_before = before.body["settings_gen"].as_i64().unwrap_or(0);

    let no = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "request_update": false })),
    )
    .await;
    assert_eq!(no.status, StatusCode::OK, "false: {}", no.body);
    assert!(
        no.body.get("update_requested_at").is_none() || no.body["update_requested_at"].is_null(),
        "request_update=false must not mark: {}",
        no.body
    );
    assert_eq!(
        no.body["settings_gen"].as_i64().unwrap_or(0),
        gen_before,
        "request_update=false must not bump gen: {}",
        no.body
    );

    let off = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "auto_update": false })),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK, "auto off: {}", off.body);
    assert_eq!(off.body["auto_update"], false, "{}", off.body);
    assert!(
        off.body.get("update_requested_at").is_none() || off.body["update_requested_at"].is_null(),
        "turning auto-update off must not request: {}",
        off.body
    );
    let gen_after_auto = off.body["settings_gen"].as_i64().unwrap_or(0);

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "request: {}", patched.body);
    assert_eq!(
        patched.body["auto_update"], false,
        "auto stays off: {}",
        patched.body
    );
    assert!(
        patched.body.get("request_update").is_none(),
        "public host has no request_update field: {}",
        patched.body
    );
    let requested = patched.body["update_requested_at"]
        .as_str()
        .unwrap_or_else(|| panic!("update_requested_at: {}", patched.body));
    assert!(
        !requested.trim().is_empty(),
        "update_requested_at must be set: {}",
        patched.body
    );
    assert_eq!(
        patched.body["settings_gen"].as_i64().unwrap_or(0),
        gen_after_auto,
        "Update now is a one-shot, not a settings bump: {}",
        patched.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after: {}", got.body);
    assert_eq!(got.body["auto_update"], false, "{}", got.body);
    assert_eq!(
        got.body["update_requested_at"].as_str(),
        Some(requested),
        "GET must show the same request: {}",
        got.body
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    assert_eq!(
        hb.body["host"]["update_requested_at"].as_str(),
        Some(requested),
        "host must see the request on the next heartbeat: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["auto_update"], false, "{}", hb.body);

    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let plane_id = fleet_db::control_plane_runner_id(ws_id);
    let plane = call(
        &app,
        "PATCH",
        &format!("{}/{plane_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(
        plane.status,
        StatusCode::NOT_FOUND,
        "synthetic plane is not a host: {}",
        plane.body
    );
}

#[tokio::test]
async fn bootstrap_conflict_without_workspace() {
    let app = app().await;
    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let out = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": "cluster-host",
            "platform": "linux-x86_64",
            "advertised_host": false,
            "advertised_container": true
        })),
    )
    .await;
    // Shared DB may already have a workspace from a parallel test.
    assert!(
        out.status == StatusCode::CONFLICT || out.status == StatusCode::OK,
        "bootstrap empty-db: {} {}",
        out.status,
        out.body
    );
    if out.status == StatusCode::CONFLICT {
        assert!(
            out.body["error"]
                .as_str()
                .unwrap_or("")
                .contains("no_workspace"),
            "{}",
            out.body
        );
    }
}

#[tokio::test]
async fn bootstrap_rejects_bad_secret() {
    let app = app().await;
    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let missing = call(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        None,
        Some(json!({
            "hostname": "x",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED, "{}", missing.body);

    let bad = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        "wrong-secret",
        Some(json!({
            "hostname": "x",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
}

/// Unset / whitespace-only FLEET_BOOTSTRAP_SECRET disables POST /bootstrap.
/// Session cookie and any Bearer (including the usual secret) must not pair.
#[tokio::test]
async fn bootstrap_secret_unset_disables_route() {
    let (app, state) = app_with_state().await;
    assert_eq!(
        state.orch.auth_mode,
        AuthMode::Token,
        "unset FLEET_BOOTSTRAP_SECRET must stay AuthMode::Token (None is not this lock)"
    );

    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let listed_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let listed_before_ids: Vec<Uuid> = listed_before.iter().map(|h| h.id).collect();
    let assignable_before = worker_hosts_assignable(&state, &ws).await;
    let hostname = format!("unset-boot-{}", Uuid::new_v4());
    let body = json!({
        "hostname": hostname,
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false
    });

    let env = BootstrapSecretEnvGuard::lock_and_set(None).await;
    for secret in [None, Some("   ")] {
        env.set(secret);
        let label = match secret {
            None => "unset",
            Some(_) => "whitespace-only",
        };

        let no_auth = call(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            None,
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(&no_auth, &format!("{label} no auth"));

        let empty_bearer = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            "",
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(
            &empty_bearer,
            &format!("{label} Authorization: Bearer (empty)"),
        );

        let bearer_x = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            "x",
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(&bearer_x, &format!("{label} Authorization: Bearer x"));

        let bearer_jwt = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            &access,
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(
            &bearer_jwt,
            &format!("{label} Authorization: Bearer <fleet_access JWT>"),
        );

        let bearer_secret = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            TEST_BOOTSTRAP_SECRET,
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(
            &bearer_secret,
            &format!("{label} Authorization: Bearer <TEST_BOOTSTRAP_SECRET>"),
        );

        let cookie_only = call(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            Some(&jar),
            Some(body.clone()),
        )
        .await;
        assert_bootstrap_secret_disabled(
            &cookie_only,
            &format!("{label} owner Cookie, no Authorization"),
        );
    }

    let listed_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let listed_after_ids: Vec<Uuid> = listed_after.iter().map(|h| h.id).collect();
    assert_eq!(
        listed_after_ids, listed_before_ids,
        "unset/whitespace bootstrap must not insert a worker_hosts row"
    );
    let assignable_after = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        assignable_after, assignable_before,
        "unset/whitespace bootstrap must not change list_assignable"
    );
    assert!(
        listed_after
            .iter()
            .all(|h| h.hostname.as_deref() != Some(hostname.as_str())),
        "disabled bootstrap must not persist hostname {hostname}: {:?}",
        listed_after
            .iter()
            .map(|h| (h.id, h.hostname.clone()))
            .collect::<Vec<_>>()
    );

    // Restore a configured secret so the 404s above are not a tautology.
    let configured = env.configured_secret();
    env.set(Some(&configured));

    let missing = call(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::UNAUTHORIZED,
        "control (bootstrap_rejects_bad_secret): missing Bearer after restore: {}",
        missing.body
    );

    let bad = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        "wrong-secret",
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        bad.status,
        StatusCode::UNAUTHORIZED,
        "control (bootstrap_rejects_bad_secret): wrong Bearer after restore: {}",
        bad.body
    );

    let ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        &configured,
        Some(json!({
            "hostname": format!("restored-boot-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": ws
        })),
    )
    .await;
    assert!(
        ok.status == StatusCode::OK || ok.status == StatusCode::CONFLICT,
        "control: configured FLEET_BOOTSTRAP_SECRET must still 200 (or 409 no_workspace): {} {}",
        ok.status,
        ok.body
    );
    if ok.status == StatusCode::CONFLICT {
        assert!(
            ok.body["error"]
                .as_str()
                .unwrap_or("")
                .contains("no_workspace"),
            "control conflict must be no_workspace: {}",
            ok.body
        );
    } else {
        let token = ok.body["machine_token"].as_str().unwrap_or("");
        assert!(
            token.starts_with("flm_"),
            "control bootstrap must mint a machine token: {}",
            ok.body
        );
        assert!(
            ok.body.get("pair_token").is_none(),
            "paired payload uses machine_token not pair_token: {}",
            ok.body
        );
    }
}

fn assert_bootstrap_secret_disabled(out: &HttpOut, label: &str) {
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "{label} POST /api/worker-hosts/bootstrap must 404 when FLEET_BOOTSTRAP_SECRET is unset/whitespace: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not 200 pair: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{label} must not 401 invalid bootstrap secret: {}",
        out.body
    );
    let hay = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        !hay.contains("machine_token"),
        "{label} body must not include machine_token: {}",
        out.raw
    );
    assert!(
        !hay.contains("pair_token"),
        "{label} body must not include pair_token: {}",
        out.raw
    );
    assert!(
        !hay.contains("control_plane_url"),
        "{label} body must not include control_plane_url: {}",
        out.raw
    );
    assert!(
        !hay.contains("invalid bootstrap secret"),
        "{label} must not say invalid bootstrap secret: {}",
        out.raw
    );
}

#[tokio::test]
async fn bootstrap_pairs_and_rotates_same_machine() {
    let app = app().await;
    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let machine_id = Uuid::new_v4();
    let first = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": "cluster-host",
            "platform": "linux-x86_64",
            "advertised_host": false,
            "advertised_container": true,
            "machine_id": machine_id,
            "workspace_id": ws
        })),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "bootstrap: {}", first.body);
    let token1 = first.body["machine_token"].as_str().unwrap();
    assert!(token1.starts_with("flm_"), "{token1}");
    assert_eq!(first.body["workspace_id"].as_str(), Some(ws.as_str()));
    let host_id = first.body["host_id"].as_str().unwrap();

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        token1,
        Some(json!({
            "advertised_host": false,
            "advertised_container": true,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);

    let again = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": "cluster-host",
            "platform": "linux-x86_64",
            "advertised_host": false,
            "advertised_container": true,
            "machine_id": machine_id,
            "workspace_id": ws
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "rotate: {}", again.body);
    let token2 = again.body["machine_token"].as_str().unwrap();
    assert_ne!(token1, token2);
    assert_eq!(again.body["host_id"].as_str(), Some(host_id));

    let old = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        token1,
        Some(json!({
            "advertised_host": false,
            "advertised_container": true,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        old.status,
        StatusCode::UNAUTHORIZED,
        "old token: {}",
        old.body
    );

    let fresh = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        token2,
        Some(json!({
            "advertised_host": false,
            "advertised_container": true,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(fresh.status, StatusCode::OK, "new token: {}", fresh.body);
}

fn assert_cluster_host_mutate_rejected(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} must be 403 (home workspace), not {}: {}",
        out.status,
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("home workspace"),
        "{what} 403 must say home workspace: {}",
        out.body
    );
    for key in ["pair_token", "machine_token"] {
        assert!(
            !json_has_key(&out.body, key),
            "{what} body must not include {key}: {}",
            out.body
        );
    }
    let hay = format!("{} {}", out.body, out.raw);
    assert!(
        !hay.contains("flp_") && !hay.contains("flm_"),
        "{what} body must not include a new pair_token / machine_token: {}",
        out.raw
    );
}

/// catalog:list-cluster-host-foreign-ws-cannot-mutate — Owner B on B's
/// path must not PATCH paused/drain/enabled/name or DELETE a bootstrap
/// cluster host. Visibility on B's GET is the documented exception.
#[tokio::test]
async fn cluster_host_foreign_workspace_cannot_pause_or_revoke() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let ws_a_id = Uuid::parse_str(&ws_a).expect("ws a");
    let hostname = format!("cluster-foreign-{}", Uuid::new_v4());

    let boot_secret = std::env::var("FLEET_BOOTSTRAP_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| TEST_BOOTSTRAP_SECRET.to_string());
    let boot = {
        let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(&boot_secret)).await;
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            &boot_secret,
            Some(json!({
                "hostname": hostname,
                "platform": "linux-x86_64",
                "advertised_host": true,
                "advertised_container": false,
                "workspace_id": ws_a
            })),
        )
        .await
    };
    assert_eq!(boot.status, StatusCode::OK, "bootstrap: {}", boot.body);
    let machine = boot.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("bootstrap machine_token: {}", boot.body))
        .to_string();
    assert!(
        machine.starts_with("flm_"),
        "bootstrap must mint flm_: {machine}"
    );
    let hid = Uuid::parse_str(
        boot.body["host_id"]
            .as_str()
            .unwrap_or_else(|| panic!("bootstrap host_id: {}", boot.body)),
    )
    .expect("host_id uuid");

    let stored = fleet_db::repos::worker_hosts::get(&state.db, hid)
        .await
        .expect("get bootstrap row")
        .unwrap_or_else(|| panic!("bootstrap must insert worker_hosts {hid}"));
    assert!(
        stored.created_by.is_none(),
        "bootstrap cluster host must have created_by null: {stored:?}"
    );
    assert!(
        stored
            .labels
            .iter()
            .any(|l| l.trim().eq_ignore_ascii_case("cluster")),
        "bootstrap must stamp labels∋cluster: {stored:?}"
    );

    let a_path = format!("{}/{hid}", hosts_path(&ws_a));
    let snap = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET after bootstrap: {}",
        snap.body
    );
    assert_eq!(snap.body["id"].as_str(), Some(hid.to_string().as_str()));
    assert!(
        labels_contain(&snap.body, "cluster"),
        "A GET labels must contain cluster: {}",
        snap.body
    );
    let original_name = snap.body["name"]
        .as_str()
        .unwrap_or_else(|| panic!("A GET name: {}", snap.body))
        .to_string();

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);

    let online = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        online.status,
        StatusCode::OK,
        "A GET after heartbeat: {}",
        online.body
    );
    assert_eq!(online.body["status"], "online", "{}", online.body);
    let original_paused = online.body["paused"].clone();
    let original_drain = online.body["drain"].clone();
    let original_enabled = online.body["enabled"].clone();
    assert_eq!(original_paused, json!(false), "{}", online.body);
    assert_eq!(original_drain, json!(false), "{}", online.body);
    assert_eq!(original_enabled, json!(true), "{}", online.body);

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    let b_path = format!("{}/{hid}", hosts_path(&ws_b));
    let b_name = format!("from-b-{}", Uuid::new_v4());

    for (label, body) in [
        ("PATCH paused", json!({ "paused": true })),
        ("PATCH drain", json!({ "drain": true })),
        ("PATCH enabled=false", json!({ "enabled": false })),
        ("PATCH name", json!({ "name": b_name })),
    ] {
        let out = call(&app, "PATCH", &b_path, Some(&jar_b), Some(body)).await;
        assert_cluster_host_mutate_rejected(&out, &format!("B {label} on B's path"));
    }

    let deleted = call(&app, "DELETE", &b_path, Some(&jar_b), None).await;
    assert_cluster_host_mutate_rejected(&deleted, "B DELETE on B's path");

    let after = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "A GET after B mutate: {}",
        after.body
    );
    assert_eq!(after.body["id"].as_str(), Some(hid.to_string().as_str()));
    assert_ne!(
        after.body["status"], "disabled",
        "status must not be revoked: {}",
        after.body
    );
    assert_eq!(
        after.body["status"], "online",
        "status must stay online: {}",
        after.body
    );
    assert_eq!(
        after.body["paused"], original_paused,
        "paused unchanged: {}",
        after.body
    );
    assert_eq!(
        after.body["drain"], original_drain,
        "drain unchanged: {}",
        after.body
    );
    assert_eq!(
        after.body["enabled"], original_enabled,
        "enabled unchanged: {}",
        after.body
    );
    assert_eq!(
        after.body["name"].as_str(),
        Some(original_name.as_str()),
        "name unchanged: {}",
        after.body
    );

    let hb_still = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(
        hb_still.status,
        StatusCode::OK,
        "A's flm_ /me/heartbeat after B mutate: {}",
        hb_still.body
    );

    let ws_b_id = Uuid::parse_str(&ws_b).expect("ws b");
    let foreign_claim = enqueue_spawn_for_host(&state, ws_b_id, hid).await;
    let foreign_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        foreign_lease.status,
        StatusCode::OK,
        "assignability /me/lease: {}",
        foreign_lease.body
    );
    assert_eq!(
        foreign_lease.body["claim"]["id"].as_str(),
        Some(foreign_claim.to_string().as_str()),
        "cluster host must still claim B's spawn_session (do not invert cluster_host_completes_claim_in_other_workspace): {}",
        foreign_lease.body
    );
    let foreign_token = foreign_lease.body["claim"]["lease_token"]
        .as_str()
        .expect("lease_token")
        .to_string();
    let foreign_done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{foreign_claim}/complete"),
        &machine,
        Some(json!({ "lease_token": foreign_token, "ok": true })),
    )
    .await;
    assert_eq!(
        foreign_done.status,
        StatusCode::OK,
        "complete B's spawn: {}",
        foreign_done.body
    );

    let rotate = call(
        &app,
        "POST",
        &format!("{b_path}/pair-tokens"),
        Some(&jar_b),
        None,
    )
    .await;
    assert_cluster_host_mutate_rejected(&rotate, "B POST pair-tokens on B's path");

    let b_get = call(&app, "GET", &b_path, Some(&jar_b), None).await;
    assert_eq!(
        b_get.status,
        StatusCode::OK,
        "B GET cluster guest must be 200: {}",
        b_get.body
    );
    assert_eq!(
        b_get.body["id"].as_str(),
        Some(hid.to_string().as_str()),
        "B GET must return the cluster host: {}",
        b_get.body
    );
    assert!(
        labels_contain(&b_get.body, "cluster"),
        "B GET labels must contain cluster: {}",
        b_get.body
    );
    for key in ["pair_token", "machine_token", "machine_token_hash"] {
        assert!(
            !json_has_key(&b_get.body, key),
            "B GET must not include {key}: {}",
            b_get.body
        );
    }
    assert!(
        !b_get.raw.contains("flp_") && !b_get.raw.contains(&machine),
        "B GET must not include pair/machine plaintext: {}",
        b_get.raw
    );

    let b_list = call(&app, "GET", &hosts_path(&ws_b), Some(&jar_b), None).await;
    assert_eq!(
        b_list.status,
        StatusCode::OK,
        "B GET /worker-hosts: {}",
        b_list.body
    );
    // Cluster hosts are GET-able from another workspace. They are not
    // injected into that workspace's list.
    if let Some(b_card) = b_list.body["hosts"].as_array().and_then(|hosts| {
        hosts
            .iter()
            .find(|h| h["id"].as_str() == Some(hid.to_string().as_str()))
    }) {
        assert!(
            labels_contain(b_card, "cluster"),
            "if B list includes the host it must stay cluster: {b_card}"
        );
    }

    let paused = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(
        paused.status,
        StatusCode::OK,
        "control: A PATCH paused on A's path: {}",
        paused.body
    );
    assert_eq!(
        paused.body["paused"], true,
        "control: A pause must persist: {}",
        paused.body
    );

    let leftover = enqueue_spawn_for_host(&state, ws_a_id, hid).await;
    let paused_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&paused_lease, "control: A PATCH paused /me/lease");
    assert_claim_unleased(
        &state,
        leftover,
        "control: paused cluster host must not consume the queued spawn",
    )
    .await;
}

/// POST /bootstrap with a guessed workspace_id must 404 and mint no cluster host.
/// Session cookie must not stand in for the bootstrap secret.
#[tokio::test]
async fn bootstrap_unknown_workspace_id_is_404_and_inserts_no_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let listed_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let listed_before_ids: Vec<Uuid> = listed_before.iter().map(|h| h.id).collect();
    let assignable_before = worker_hosts_assignable(&state, &ws).await;

    let get_before = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        get_before.status,
        StatusCode::OK,
        "snapshot GET /worker-hosts: {}",
        get_before.body
    );
    let get_before_hosts = get_before.body["hosts"]
        .as_array()
        .expect("hosts array")
        .clone();
    let get_before_count = get_before_hosts.len();
    let get_before_ids: Vec<String> = get_before_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    let get_before_owned: Vec<String> = get_before_hosts
        .iter()
        .filter(|h| h["workspace_id"].as_str() == Some(ws.as_str()))
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();

    let unknown = Uuid::new_v4();
    assert_ne!(
        unknown, workspace_id,
        "garbage workspace_id must not be A's workspace"
    );
    let hostname = format!("unknown-ws-boot-{}", Uuid::new_v4());
    let body = json!({
        "hostname": hostname,
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false,
        "workspace_id": unknown
    });

    let boot_secret = std::env::var("FLEET_BOOTSTRAP_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| TEST_BOOTSTRAP_SECRET.to_string());
    let _env = BootstrapSecretEnvGuard::lock_and_set(Some(&boot_secret)).await;

    let unknown_ws = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        &boot_secret,
        Some(body.clone()),
    )
    .await;
    assert_bootstrap_unknown_workspace_rejected(
        &unknown_ws,
        &listed_before_ids,
        "Bearer configured FLEET_BOOTSTRAP_SECRET + unknown workspace_id",
    );

    let cookie_only = call(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        Some(&jar),
        Some(body.clone()),
    )
    .await;
    assert!(
        cookie_only.status == StatusCode::UNAUTHORIZED
            || cookie_only.status == StatusCode::NOT_FOUND,
        "cookie-only with garbage workspace_id must 401/404 (session is not the bootstrap secret): {} {}",
        cookie_only.status,
        cookie_only.body
    );
    assert_ne!(
        cookie_only.status,
        StatusCode::OK,
        "cookie-only must not 200 pair: {}",
        cookie_only.body
    );
    assert_ne!(
        cookie_only.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "cookie-only must not 500: {}",
        cookie_only.body
    );
    assert_bootstrap_pair_material_absent(
        &cookie_only,
        &listed_before_ids,
        "cookie-only with garbage workspace_id",
    );

    let listed_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let listed_after_ids: Vec<Uuid> = listed_after.iter().map(|h| h.id).collect();
    assert_eq!(
        listed_after_ids, listed_before_ids,
        "unknown workspace_id bootstrap must not insert a worker_hosts row in A"
    );
    let assignable_after = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        assignable_after, assignable_before,
        "unknown workspace_id bootstrap must not change list_assignable(A)"
    );
    assert!(
        listed_after
            .iter()
            .all(|h| h.hostname.as_deref() != Some(hostname.as_str()) && h.name != hostname),
        "unknown workspace_id bootstrap must not persist hostname {hostname} on A: {:?}",
        listed_after
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone()))
            .collect::<Vec<_>>()
    );

    let ghost = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, unknown)
        .await
        .expect("list_for_workspace(unknown)");
    assert!(
        ghost.is_empty(),
        "guessed workspace_id must not mint a worker_hosts row: {:?}",
        ghost
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone(), h.labels.clone()))
            .collect::<Vec<_>>()
    );

    let cluster = fleet_db::repos::worker_hosts::list_cluster(&state.db)
        .await
        .expect("list_cluster");
    assert!(
        cluster
            .iter()
            .all(|h| h.hostname.as_deref() != Some(hostname.as_str()) && h.name != hostname),
        "guessed workspace_id must not mint a cluster card named {hostname}: {:?}",
        cluster
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone(), h.workspace_id))
            .collect::<Vec<_>>()
    );

    let get_after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        get_after.status,
        StatusCode::OK,
        "GET /worker-hosts after unknown bootstrap: {}",
        get_after.body
    );
    let get_after_hosts = get_after.body["hosts"].as_array().expect("hosts array");
    assert!(
        get_after_hosts.iter().all(|h| {
            h["hostname"].as_str() != Some(hostname.as_str())
                && h["name"].as_str() != Some(hostname.as_str())
                && h["workspace_id"].as_str() != Some(&unknown.to_string())
        }),
        "GET /worker-hosts must not show a new cluster card for the guessed workspace: {}",
        get_after.body
    );
    let get_after_owned: Vec<String> = get_after_hosts
        .iter()
        .filter(|h| h["workspace_id"].as_str() == Some(ws.as_str()))
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        get_after_owned, get_before_owned,
        "GET /worker-hosts workspace-A cards must stay the snapshot"
    );
    let get_after_ids: Vec<String> = get_after_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    let new_get_ids: Vec<&String> = get_after_ids
        .iter()
        .filter(|id| !get_before_ids.contains(id))
        .collect();
    assert!(
        new_get_ids.iter().all(|id| {
            get_after_hosts.iter().any(|h| {
                h["id"].as_str() == Some(id.as_str())
                    && h["workspace_id"].as_str() != Some(&unknown.to_string())
                    && h["hostname"].as_str() != Some(hostname.as_str())
                    && h["name"].as_str() != Some(hostname.as_str())
            })
        }),
        "GET /worker-hosts grew by a guessed-workspace cluster card: before={get_before_count} after={} body={}",
        get_after_hosts.len(),
        get_after.body
    );

    let ok = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        &boot_secret,
        Some(json!({
            "hostname": format!("known-ws-boot-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": ws
        })),
    )
    .await;
    assert_eq!(
        ok.status,
        StatusCode::OK,
        "control: same secret + workspace_id=A must still 200: {}",
        ok.body
    );
    assert_eq!(
        ok.body["workspace_id"].as_str(),
        Some(ws.as_str()),
        "control bootstrap must pin workspace A: {}",
        ok.body
    );
    let token = ok.body["machine_token"].as_str().unwrap_or("");
    assert!(
        token.starts_with("flm_"),
        "control bootstrap must mint a machine token: {}",
        ok.body
    );
    assert!(
        ok.body.get("pair_token").is_none(),
        "paired payload uses machine_token not pair_token: {}",
        ok.body
    );
}

fn assert_bootstrap_unknown_workspace_rejected(
    out: &HttpOut,
    existing_host_ids: &[Uuid],
    label: &str,
) {
    let hay = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    let not_found_400 = out.status == StatusCode::BAD_REQUEST
        && (hay.contains("not found") || hay.contains("not-found") || hay.contains("not_found"));
    assert!(
        out.status == StatusCode::NOT_FOUND || not_found_400,
        "{label} POST /api/worker-hosts/bootstrap must 404 (or 400 not-found), not {} : {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not 200 pair: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{label} must not 500: {}",
        out.body
    );
    assert_bootstrap_pair_material_absent(out, existing_host_ids, label);
}

fn assert_bootstrap_pair_material_absent(out: &HttpOut, existing_host_ids: &[Uuid], label: &str) {
    let hay = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        !hay.contains("machine_token"),
        "{label} body must not include machine_token: {}",
        out.raw
    );
    assert!(
        !hay.contains("pair_token"),
        "{label} body must not include pair_token: {}",
        out.raw
    );
    assert!(
        !hay.contains("control_plane_url"),
        "{label} body must not include control_plane_url: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("flm_"),
        "{label} must not mint flm_: {}",
        out.raw
    );
    if let Some(hid) = out
        .body
        .get("host_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let known = Uuid::parse_str(hid)
            .ok()
            .is_some_and(|id| existing_host_ids.contains(&id));
        assert!(
            known,
            "{label} body must not include host_id of a new row: {}",
            out.body
        );
    }
}

fn install_request_spec(body: &Value) -> Option<&str> {
    body.get("install_request")
        .and_then(|v| v.get("spec"))
        .and_then(|v| v.as_str())
}

fn assert_install_request_cleared(body: &Value, what: &str) {
    assert!(
        body.get("install_request").is_none() || body["install_request"].is_null(),
        "{what} must leave install_request null (matching ack is allowed to clear): {body}"
    );
}

/// catalog:unit-install-ack — heartbeat `install_ack` without the queued spec
/// must not drop Apply/Unlisted. Matching spec may clear.
#[tokio::test]
async fn install_ack_mismatch_keeps_queued_request() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let queued = "llama3.2:latest";
    let queued_body = json!({
        "install_model": { "provider": "ollama", "spec": queued }
    });

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(queued_body.clone()),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH install_model: {}",
        patched.body
    );
    assert_eq!(
        install_request_spec(&patched.body),
        Some(queued),
        "PATCH must queue the spec: {}",
        patched.body
    );

    let snap = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "GET after queue: {}",
        snap.body
    );
    assert_eq!(
        install_request_spec(&snap.body),
        Some(queued),
        "GET /worker-hosts/{{hid}} install_request.spec must be the queued string: {}",
        snap.body
    );

    let member_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&member_jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": "member-model:latest" }
        })),
    )
    .await;
    assert_eq!(
        member_patch.status,
        StatusCode::FORBIDDEN,
        "member PATCH install_model must stay 403: {}",
        member_patch.body
    );

    let other_ws_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&other_jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": "other-ws-model:latest" }
        })),
    )
    .await;
    assert_eq!(
        other_ws_patch.status,
        StatusCode::FORBIDDEN,
        "other-ws PATCH install_model must stay 403: {}",
        other_ws_patch.body
    );
    let other_ws_own = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(queued_body.clone()),
    )
    .await;
    assert_ne!(
        other_ws_own.status,
        StatusCode::OK,
        "other workspace path must not queue install_model on this hid: {}",
        other_ws_own.body
    );
    assert_ne!(
        other_ws_own.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "other workspace PATCH install_model must not 5xx: {}",
        other_ws_own.body
    );

    let after_denied = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_denied.status,
        StatusCode::OK,
        "GET after denied PATCH: {}",
        after_denied.body
    );
    assert_eq!(
        install_request_spec(&after_denied.body),
        Some(queued),
        "member/other-ws must not replace the queued spec: {}",
        after_denied.body
    );

    let cookie_hb = call(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        Some(&jar),
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "install_ack": { "ok": true, "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        cookie_hb.status,
        StatusCode::UNAUTHORIZED,
        "cookie-only heartbeat must stay 401: {}",
        cookie_hb.body
    );

    let after_cookie = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_cookie.status,
        StatusCode::OK,
        "GET after cookie heartbeat: {}",
        after_cookie.body
    );
    assert_eq!(
        install_request_spec(&after_cookie.body),
        Some(queued),
        "cookie-only heartbeat must not clear install_request: {}",
        after_cookie.body
    );

    let mismatch_acks = [
        ("omitted spec", json!({ "ok": true })),
        ("empty spec", json!({ "ok": true, "spec": "" })),
        ("whitespace spec", json!({ "ok": true, "spec": "   " })),
        (
            "other spec",
            json!({ "ok": true, "spec": "other-model:latest" }),
        ),
    ];
    for (label, ack) in mismatch_acks {
        let hb = call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/me/heartbeat",
            &machine,
            Some(json!({
                "advertised_host": true,
                "advertised_container": false,
                "current_jobs": 0,
                "install_ack": ack
            })),
        )
        .await;
        assert_ne!(
            hb.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "install_ack {label} must not 5xx: {}",
            hb.body
        );
        assert_eq!(
            hb.status,
            StatusCode::OK,
            "install_ack {label} must 200: {}",
            hb.body
        );
        let still = call(&app, "GET", &host_path, Some(&jar), None).await;
        assert_eq!(
            still.status,
            StatusCode::OK,
            "GET after install_ack {label}: {}",
            still.body
        );
        assert_eq!(
            install_request_spec(&still.body),
            Some(queued),
            "install_ack {label} must leave the original install_request.spec (omit/empty/wrong spec must not drop Apply/Unlisted): {}",
            still.body
        );
    }

    let match_hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "install_ack": { "ok": true, "spec": queued }
        })),
    )
    .await;
    assert_ne!(
        match_hb.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "matching install_ack must not 5xx: {}",
        match_hb.body
    );
    assert_eq!(
        match_hb.status,
        StatusCode::OK,
        "matching install_ack may 200: {}",
        match_hb.body
    );
    let after_match = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_match.status,
        StatusCode::OK,
        "GET after matching ack: {}",
        after_match.body
    );
    assert_install_request_cleared(
        &after_match.body,
        "matching install_ack {ok, spec: llama3.2:latest}",
    );

    let second = "qwen2.5:latest";
    let second_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": second }
        })),
    )
    .await;
    assert_eq!(
        second_patch.status,
        StatusCode::OK,
        "second PATCH install_model: {}",
        second_patch.body
    );
    let after_second = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_second.status,
        StatusCode::OK,
        "GET after second queue: {}",
        after_second.body
    );
    assert_eq!(
        install_request_spec(&after_second.body),
        Some(second),
        "second queued spec: {}",
        after_second.body
    );

    let old_spec_ack = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "install_ack": { "ok": true, "spec": queued }
        })),
    )
    .await;
    assert_ne!(
        old_spec_ack.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "ack of the previous spec must not 5xx: {}",
        old_spec_ack.body
    );
    assert_eq!(
        old_spec_ack.status,
        StatusCode::OK,
        "ack of the previous spec must 200: {}",
        old_spec_ack.body
    );
    let still_second = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        still_second.status,
        StatusCode::OK,
        "GET after old-spec ack: {}",
        still_second.body
    );
    assert_eq!(
        install_request_spec(&still_second.body),
        Some(second),
        "matching ack may clear only the currently queued spec, not a previous one: {}",
        still_second.body
    );

    let second_match = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "install_ack": { "ok": true, "spec": second }
        })),
    )
    .await;
    assert_eq!(
        second_match.status,
        StatusCode::OK,
        "matching ack of the second spec may 200: {}",
        second_match.body
    );
    let after_second_match = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_second_match.status,
        StatusCode::OK,
        "GET after second matching ack: {}",
        after_second_match.body
    );
    assert_install_request_cleared(
        &after_second_match.body,
        "matching install_ack of the second queued spec",
    );
}

/// catalog:list-install-model-rejects-shell-metachar — PATCH
/// install_model.spec empty / non-ollama / >512 / control / ;|& is 400
/// (looks unsafe / only ollama) and does not persist. A safe spec
/// (llama3.2:latest) may persist. Member and other-ws 403. Plane id
/// must not accept install_model. Distinct from
/// list-local-models-disabled-no-proxy (catalog must not hit :1234).
/// Do not weaken the charset gate to keep a looser match green — `;`,
/// `|`, and `&` are each independently 400.
fn assert_install_model_400(out: &HttpOut, what: &str, err_needles: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "install_model {what} must not 5xx: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "install_model {what} must not persist-and-200: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "install_model {what} must be 400: {}",
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    for needle in err_needles {
        assert!(
            err.contains(needle),
            "install_model {what} 400 must mention {needle:?} (looks unsafe / only ollama): {}",
            out.body
        );
    }
    assert!(
        install_request_spec(&out.body).is_none(),
        "400 must not look applied (install_request.spec set): {}",
        out.body
    );
}

async fn assert_install_request_is(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    want: Option<&str>,
    what: &str,
) {
    let got = call(app, "GET", path, Some(jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{what} GET: {}", got.body);
    assert_eq!(
        install_request_spec(&got.body),
        want,
        "{what} GET install_request.spec must be {want:?}: {}",
        got.body
    );
}

async fn assert_get_omits_install_needles(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    needles: &[&str],
    what: &str,
) {
    let got = call(app, "GET", path, Some(jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{what} GET: {}", got.body);
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        assert!(
            !got.raw.contains(needle),
            "{what} GET must not persist rejected install_model needle {needle:?}: {}",
            got.raw
        );
    }
}

#[tokio::test]
async fn install_model_rejects_shell_metachar_empty_and_non_ollama() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let host_list = hosts_path(&ws);

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    assert!(
        install_request_spec(&seed.body).is_none(),
        "paired host starts with no install_request: {}",
        seed.body
    );

    let oversize: String = "oversize-install-"
        .chars()
        .chain(std::iter::repeat('x'))
        .take(513)
        .collect();
    assert_eq!(oversize.len(), 513, "oversize spec must be 513 bytes");

    let rejects: &[(&str, Value, &[&str], &[&str])] = &[
        (
            "empty spec",
            json!({ "install_model": { "provider": "ollama", "spec": "" } }),
            &["required"],
            &[],
        ),
        (
            "whitespace spec",
            json!({ "install_model": { "provider": "ollama", "spec": "   " } }),
            &["required"],
            &[],
        ),
        (
            "missing spec",
            json!({ "install_model": { "provider": "ollama" } }),
            &["required"],
            &[],
        ),
        (
            "provider grok",
            json!({ "install_model": { "provider": "grok", "spec": "grok-4-install-unique" } }),
            &["ollama"],
            &["grok-4-install-unique"],
        ),
        (
            "provider openai",
            json!({ "install_model": { "provider": "openai", "spec": "gpt-oss-install-unique" } }),
            &["ollama"],
            &["gpt-oss-install-unique"],
        ),
        (
            "empty provider",
            json!({ "install_model": { "provider": "", "spec": "empty-provider-install-unique" } }),
            &["ollama"],
            &["empty-provider-install-unique"],
        ),
        (
            "missing provider",
            json!({ "install_model": { "spec": "missing-provider-install-unique" } }),
            &["ollama"],
            &["missing-provider-install-unique"],
        ),
        (
            "spec 513 bytes",
            json!({ "install_model": { "provider": "ollama", "spec": oversize } }),
            &["looks unsafe"],
            &["oversize-install-"],
        ),
        (
            "embedded newline",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest\ncurl-evil-install" } }),
            &["looks unsafe"],
            &["curl-evil-install"],
        ),
        (
            "embedded nul",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest\u{0000}nul-install" } }),
            &["looks unsafe"],
            &["nul-install"],
        ),
        (
            "semicolon",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest;rm-install" } }),
            &["looks unsafe"],
            &[";rm-install"],
        ),
        (
            "pipe",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest|curl-install" } }),
            &["looks unsafe"],
            &["|curl-install"],
        ),
        (
            "ampersand",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest&id-install" } }),
            &["looks unsafe"],
            &["&id-install"],
        ),
    ];

    for (label, body, err_needles, persist_needles) in rejects {
        let rejected = call(&app, "PATCH", &host_path, Some(&jar), Some(body.clone())).await;
        assert_install_model_400(&rejected, label, err_needles);
        assert_install_request_is(
            &app,
            &jar,
            &host_path,
            None,
            &format!("after owner reject {label} (no prior queue)"),
        )
        .await;
        assert_get_omits_install_needles(
            &app,
            &jar,
            &host_path,
            persist_needles,
            &format!("after owner reject {label}"),
        )
        .await;
    }

    let listed_empty = call(&app, "GET", &host_list, Some(&jar), None).await;
    assert_eq!(
        listed_empty.status,
        StatusCode::OK,
        "list after rejects: {}",
        listed_empty.body
    );
    let listed_host = listed_empty.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(&host_id.to_string()))
        .unwrap_or_else(|| panic!("list missing host: {}", listed_empty.body));
    assert!(
        install_request_spec(listed_host).is_none(),
        "list must not queue a rejected install_model: {listed_host}"
    );

    let safe = "llama3.2:latest";
    let persist = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": safe }
        })),
    )
    .await;
    assert_eq!(
        persist.status,
        StatusCode::OK,
        "safe PATCH install_model llama3.2:latest may persist: {}",
        persist.body
    );
    assert_eq!(
        install_request_spec(&persist.body),
        Some(safe),
        "safe PATCH must queue llama3.2:latest: {}",
        persist.body
    );
    assert_install_request_is(&app, &jar, &host_path, Some(safe), "GET after safe persist").await;

    for (label, body, err_needles, persist_needles) in rejects {
        let rejected = call(&app, "PATCH", &host_path, Some(&jar), Some(body.clone())).await;
        assert_install_model_400(&rejected, &format!("{label} after persist"), err_needles);
        assert_install_request_is(
            &app,
            &jar,
            &host_path,
            Some(safe),
            &format!("after reject {label} following persist {safe}"),
        )
        .await;
        assert_get_omits_install_needles(
            &app,
            &jar,
            &host_path,
            persist_needles,
            &format!("after reject {label} following persist"),
        )
        .await;
    }

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "member me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    for (label, body) in [
        (
            "safe spec",
            json!({ "install_model": { "provider": "ollama", "spec": "member-model:latest" } }),
        ),
        (
            "semicolon",
            json!({ "install_model": { "provider": "ollama", "spec": "member;rm-install" } }),
        ),
    ] {
        let member_patch = call(&app, "PATCH", &host_path, Some(&member_jar), Some(body)).await;
        assert_eq!(
            member_patch.status,
            StatusCode::FORBIDDEN,
            "member PATCH install_model {label} must stay 403: {}",
            member_patch.body
        );
    }

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    for (label, body) in [
        (
            "safe spec",
            json!({ "install_model": { "provider": "ollama", "spec": "other-ws-model:latest" } }),
        ),
        (
            "pipe",
            json!({ "install_model": { "provider": "ollama", "spec": "other|curl-install" } }),
        ),
    ] {
        let other_patch = call(&app, "PATCH", &host_path, Some(&other_jar), Some(body)).await;
        assert_eq!(
            other_patch.status,
            StatusCode::FORBIDDEN,
            "other-ws PATCH install_model {label} must stay 403: {}",
            other_patch.body
        );
    }
    let other_ws_own = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": safe }
        })),
    )
    .await;
    assert_ne!(
        other_ws_own.status,
        StatusCode::OK,
        "other workspace path must not queue install_model on this hid: {}",
        other_ws_own.body
    );
    assert_ne!(
        other_ws_own.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "other workspace PATCH install_model must not 5xx: {}",
        other_ws_own.body
    );

    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        Some(safe),
        "after member/other-ws 403",
    )
    .await;
    assert_get_omits_install_needles(
        &app,
        &jar,
        &host_path,
        &[
            "member-model:latest",
            "member;rm-install",
            "other-ws-model:latest",
            "other|curl-install",
        ],
        "after member/other-ws 403",
    )
    .await;
}

/// catalog:list-runtime-settings-rejects-shell-metachar — Owner PATCH
/// runtime_settings with ;|& is 400 looks unsafe and does not persist.
/// Unknown keys (extra_env, machine_token, FLEET_WORKER_TOKEN) must not
/// appear on GET runtime_settings. A safe work_dir may persist so this
/// is not a tautology that every PATCH 400s. POST /me/heartbeat with
/// runtime tokens/extra_env/;|& is 200; GET runtime_reported must not
/// echo unique-flm / unique-secret / extra_env / ;|&. Desired
/// runtime_settings is unchanged (reported is not apply). Member and
/// other-ws PATCH stay 403. Plane id must not invent a worker_hosts
/// runtime_settings row. Distinct from
/// list-install-model-rejects-shell-metachar and
/// list-heartbeat-agents-omit-tokens. Do not keep a validate_runtime_path
/// unit as the only lock.
fn assert_runtime_settings_400(out: &HttpOut, what: &str) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "runtime_settings {what} must not 5xx: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "runtime_settings {what} must not persist-and-200: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "runtime_settings {what} must be 400: {}",
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("looks unsafe"),
        "runtime_settings {what} 400 must mention looks unsafe: {}",
        out.body
    );
}

fn runtime_field<'a>(body: &'a Value, key: &str) -> &'a Value {
    body.get(key).unwrap_or(&Value::Null)
}

fn runtime_blob(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn assert_runtime_omits_metachar(v: &Value, what: &str) {
    let blob = runtime_blob(v);
    for ch in [';', '|', '&'] {
        assert!(!blob.contains(ch), "{what} must not contain {ch:?}: {blob}");
    }
}

fn assert_runtime_omits_secret_keys(v: &Value, what: &str) {
    if v.is_null() {
        return;
    }
    for key in [
        "extra_env",
        "machine_token",
        "FLEET_WORKER_TOKEN",
        "pair_token",
    ] {
        assert!(
            v.get(key).is_none() && !json_has_key(v, key),
            "{what} must not persist {key}: {v}"
        );
    }
}

fn assert_get_runtime_settings_omits(out: &HttpOut, needles: &[&str], what: &str) {
    assert_eq!(out.status, StatusCode::OK, "{what} GET: {}", out.body);
    let settings = runtime_field(&out.body, "runtime_settings");
    assert_runtime_omits_metachar(settings, &format!("{what} GET runtime_settings"));
    assert_runtime_omits_secret_keys(settings, &format!("{what} GET runtime_settings"));
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        assert!(
            !out.raw.contains(needle) && !json_contains_str(&out.body, needle),
            "{what} GET must not contain {needle:?}: {}",
            out.raw
        );
        assert!(
            !runtime_blob(settings).contains(needle),
            "{what} GET runtime_settings must not contain {needle:?}: {settings}"
        );
    }
}

#[tokio::test]
async fn runtime_settings_rejects_shell_metachar_and_heartbeat_omits_secrets() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let hid = host_id.to_string();
    let host_path = format!("{}/{hid}", hosts_path(&ws));
    let host_list = hosts_path(&ws);
    let plane_id = fleet_db::control_plane_runner_id(workspace_id);

    let unsafe_work_dir = "/tmp;rm -rf /";
    let unsafe_docker_image = "img|curl evil";
    let unsafe_ollama_dir = "/var/x&y";
    let safe_work_dir = "/var/lib/fleet-host/work";
    let stamp = Uuid::new_v4();
    let unique_flm = format!("unique-flm-{stamp}");
    let unique_secret = format!("unique-secret-{stamp}");

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    assert_get_runtime_settings_omits(
        &seed,
        &[unsafe_work_dir, unsafe_docker_image, unsafe_ollama_dir],
        "seed",
    );
    let seed_settings = runtime_field(&seed.body, "runtime_settings").clone();

    let combined = json!({
        "runtime_settings": {
            "work_dir": unsafe_work_dir,
            "docker_image": unsafe_docker_image,
            "ollama_models_dir": unsafe_ollama_dir,
        }
    });
    let rejected = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(combined.clone()),
    )
    .await;
    assert_runtime_settings_400(&rejected, "combined ;|&");
    let after_combined = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_get_runtime_settings_omits(
        &after_combined,
        &[unsafe_work_dir, unsafe_docker_image, unsafe_ollama_dir],
        "after combined reject",
    );
    assert_eq!(
        runtime_field(&after_combined.body, "runtime_settings"),
        &seed_settings,
        "combined reject must not write runtime_settings: {}",
        after_combined.body
    );

    let independent: &[(&str, Value, &str)] = &[
        (
            "work_dir semicolon",
            json!({ "runtime_settings": { "work_dir": unsafe_work_dir } }),
            unsafe_work_dir,
        ),
        (
            "docker_image pipe",
            json!({ "runtime_settings": { "docker_image": unsafe_docker_image } }),
            unsafe_docker_image,
        ),
        (
            "ollama_models_dir ampersand",
            json!({ "runtime_settings": { "ollama_models_dir": unsafe_ollama_dir } }),
            unsafe_ollama_dir,
        ),
    ];
    for (label, body, needle) in independent {
        let rejected = call(&app, "PATCH", &host_path, Some(&jar), Some(body.clone())).await;
        assert_runtime_settings_400(&rejected, label);
        let got = call(&app, "GET", &host_path, Some(&jar), None).await;
        assert_get_runtime_settings_omits(&got, &[needle], &format!("after {label} reject"));
        assert_eq!(
            runtime_field(&got.body, "runtime_settings"),
            &seed_settings,
            "{label} reject must not write runtime_settings: {}",
            got.body
        );
    }

    let unknown = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "runtime_settings": {
                "extra_env": { "FLEET_WORKER_TOKEN": unique_flm, "SECRET": unique_secret },
                "machine_token": unique_flm,
                "FLEET_WORKER_TOKEN": unique_flm,
            }
        })),
    )
    .await;
    assert_ne!(
        unknown.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PATCH unknown runtime_settings keys must not 5xx: {}",
        unknown.body
    );
    let after_unknown = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_get_runtime_settings_omits(
        &after_unknown,
        &[&unique_flm, &unique_secret],
        "after unknown-key PATCH",
    );
    let unknown_settings = runtime_field(&after_unknown.body, "runtime_settings");
    assert_runtime_omits_secret_keys(unknown_settings, "GET after unknown-key PATCH");
    assert!(
        after_unknown.body.get("runtime_settings").is_none()
            || after_unknown.body["runtime_settings"]
                .as_object()
                .is_none_or(|o| {
                    !o.contains_key("extra_env")
                        && !o.contains_key("machine_token")
                        && !o.contains_key("FLEET_WORKER_TOKEN")
                }),
        "GET runtime_settings must not persist unknown keys: {}",
        after_unknown.body
    );

    let persist = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "runtime_settings": { "work_dir": safe_work_dir } })),
    )
    .await;
    assert_eq!(
        persist.status,
        StatusCode::OK,
        "safe PATCH runtime_settings work_dir may persist: {}",
        persist.body
    );
    assert_eq!(
        persist.body["runtime_settings"]["work_dir"].as_str(),
        Some(safe_work_dir),
        "safe PATCH must store work_dir: {}",
        persist.body
    );
    let after_safe = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_safe.status,
        StatusCode::OK,
        "GET after safe persist: {}",
        after_safe.body
    );
    assert_eq!(
        after_safe.body["runtime_settings"]["work_dir"].as_str(),
        Some(safe_work_dir),
        "GET must show persisted work_dir so this is not a tautology that every PATCH 400s: {}",
        after_safe.body
    );
    assert_runtime_omits_secret_keys(
        runtime_field(&after_safe.body, "runtime_settings"),
        "GET after safe persist",
    );
    let desired_settings = runtime_field(&after_safe.body, "runtime_settings").clone();

    let listed_safe = call(&app, "GET", &host_list, Some(&jar), None).await;
    assert_eq!(
        listed_safe.status,
        StatusCode::OK,
        "list after safe persist: {}",
        listed_safe.body
    );
    let listed_host = list_host_by_id(&listed_safe, &hid, "after safe persist");
    assert_eq!(
        listed_host["runtime_settings"]["work_dir"].as_str(),
        Some(safe_work_dir),
        "list must persist work_dir: {listed_host}"
    );

    let rejected_after = call(&app, "PATCH", &host_path, Some(&jar), Some(combined)).await;
    assert_runtime_settings_400(&rejected_after, "combined ;|& after persist");
    let still_safe = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        runtime_field(&still_safe.body, "runtime_settings"),
        &desired_settings,
        "reject after persist must keep desired runtime_settings: {}",
        still_safe.body
    );
    assert_get_runtime_settings_omits(
        &still_safe,
        &[unsafe_work_dir, unsafe_docker_image, unsafe_ollama_dir],
        "after combined reject following persist",
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "runtime": {
                "work_dir": "/tmp;id",
                "extra_env": {
                    "FLEET_WORKER_TOKEN": unique_flm,
                    "SECRET": unique_secret,
                },
                "machine_token": unique_flm,
            }
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat with runtime must 200: {}",
        hb.body
    );

    let after_hb = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_hb.status,
        StatusCode::OK,
        "GET after heartbeat runtime: {}",
        after_hb.body
    );
    assert_eq!(
        runtime_field(&after_hb.body, "runtime_settings"),
        &desired_settings,
        "heartbeat runtime must not apply onto desired runtime_settings: {}",
        after_hb.body
    );
    let reported = runtime_field(&after_hb.body, "runtime_reported");
    assert_runtime_omits_metachar(reported, "GET runtime_reported after heartbeat");
    assert_runtime_omits_secret_keys(reported, "GET runtime_reported after heartbeat");
    for needle in [unique_flm.as_str(), unique_secret.as_str(), "extra_env"] {
        assert!(
            !runtime_blob(reported).contains(needle)
                && !json_contains_str(reported, needle)
                && (needle == "extra_env" || !after_hb.raw.contains(needle)),
            "GET runtime_reported must not contain {needle:?}: {}",
            after_hb.raw
        );
    }
    if let Some(wd) = reported.get("work_dir").and_then(|v| v.as_str()) {
        assert!(
            !wd.contains(';') && !wd.contains('|') && !wd.contains('&'),
            "allowlisted work_dir without metachar may appear; {wd:?} is unsafe: {reported}"
        );
    }
    let listed_hb = call(&app, "GET", &host_list, Some(&jar), None).await;
    assert_eq!(
        listed_hb.status,
        StatusCode::OK,
        "list after heartbeat: {}",
        listed_hb.body
    );
    let listed_after_hb = list_host_by_id(&listed_hb, &hid, "after heartbeat");
    assert_eq!(
        runtime_field(listed_after_hb, "runtime_settings"),
        &desired_settings,
        "list desired runtime_settings must stay the safe work_dir: {listed_after_hb}"
    );
    let listed_reported = runtime_field(listed_after_hb, "runtime_reported");
    assert_runtime_omits_metachar(listed_reported, "list runtime_reported after heartbeat");
    assert_runtime_omits_secret_keys(listed_reported, "list runtime_reported after heartbeat");
    for needle in [unique_flm.as_str(), unique_secret.as_str(), "extra_env"] {
        assert!(
            !runtime_blob(listed_reported).contains(needle)
                && !json_contains_str(listed_reported, needle),
            "list runtime_reported must not contain {needle:?}: {listed_after_hb}"
        );
    }

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "member me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        workspace_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    for (label, body) in [
        (
            "safe work_dir",
            json!({ "runtime_settings": { "work_dir": "/tmp/member-runtime" } }),
        ),
        (
            "semicolon",
            json!({ "runtime_settings": { "work_dir": "/tmp;rm-member" } }),
        ),
    ] {
        let member_patch = call(&app, "PATCH", &host_path, Some(&member_jar), Some(body)).await;
        assert_eq!(
            member_patch.status,
            StatusCode::FORBIDDEN,
            "member PATCH runtime_settings {label} must stay 403: {}",
            member_patch.body
        );
    }

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    assert_ne!(ws, other_ws, "A and B must have distinct workspaces");
    for (label, body) in [
        (
            "safe work_dir",
            json!({ "runtime_settings": { "work_dir": "/tmp/other-ws-runtime" } }),
        ),
        (
            "pipe",
            json!({ "runtime_settings": { "docker_image": "img|curl-other" } }),
        ),
    ] {
        let other_patch = call(&app, "PATCH", &host_path, Some(&other_jar), Some(body)).await;
        assert_eq!(
            other_patch.status,
            StatusCode::FORBIDDEN,
            "other-ws PATCH runtime_settings {label} must stay 403: {}",
            other_patch.body
        );
    }
    let other_ws_own = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(json!({ "runtime_settings": { "work_dir": safe_work_dir } })),
    )
    .await;
    assert_ne!(
        other_ws_own.status,
        StatusCode::OK,
        "other workspace path must not write runtime_settings on this hid: {}",
        other_ws_own.body
    );
    assert_ne!(
        other_ws_own.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "other workspace PATCH runtime_settings must not 5xx: {}",
        other_ws_own.body
    );

    let after_acl = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        runtime_field(&after_acl.body, "runtime_settings"),
        &desired_settings,
        "member/other-ws 403 must leave desired runtime_settings: {}",
        after_acl.body
    );
    assert_get_runtime_settings_omits(
        &after_acl,
        &[
            "/tmp/member-runtime",
            "/tmp;rm-member",
            "/tmp/other-ws-runtime",
            "img|curl-other",
        ],
        "after member/other-ws 403",
    );

    assert_not_a_hosts_row(&state, workspace_id, plane_id).await;
}

#[tokio::test]
async fn local_models_requires_auth_including_local_model_tags() {
    let (app, _) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let path = format!("/api/workspaces/{ws}/local-models");
    let anon = call(&app, "GET", &path, None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{}", anon.body);
    // Do not assert the 200 catalog body here: catalog() may hit ollama.com.
    let tags = format!("/api/workspaces/{ws}/local-models/llama3.2/tags");
    let anon_tags = call(&app, "GET", &tags, None, None).await;
    assert_eq!(
        anon_tags.status,
        StatusCode::UNAUTHORIZED,
        "unauth GET /local-models/llama3.2/tags must be 401: {}",
        anon_tags.body
    );
    let _ = jar;
}

/// catalog:list-local-model-tags-rejects-unsafe-slug — member GET
/// `/local-models/{name}/tags` is 400 for path/metachar / empty / 129-char
/// slugs (not 200 `unavailable`, not 500) and must not open ollama.com or
/// :1234. GET after reject stays 400 (no tags cache for the bad name).
/// Other-ws cookie on A's catalog and tags is 403 not-a-member and must
/// not return a catalog/tags body. Unauth tags is 401. A safe slug
/// (`llama3.2`) may 200 with source cache | ollama.com/library/llama3.2/tags
/// | unavailable — must not 400 as unknown provider. Distinct from
/// list-local-models-disabled-no-proxy (`catalog()` must not hit :1234)
/// and list-install-model-rejects-shell-metachar (PATCH `install_model`).
/// Keep `slug_rejects_paths` (helper unit, not HTTP).
fn encode_local_model_slug(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn local_models_catalog_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/local-models")
}

fn local_model_tags_path(ws: &str, name: &str) -> String {
    format!(
        "/api/workspaces/{ws}/local-models/{}/tags",
        encode_local_model_slug(name)
    )
}

fn assert_local_model_tags_400(out: &HttpOut, what: &str) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "GET {what} must not 500: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "GET {what} must not 200 unavailable (HTTP gate, not tags_for miss): {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "GET {what} must be 400: {}",
        out.body
    );
    assert!(
        out.body.get("tags").and_then(Value::as_array).is_none(),
        "400 {what} must not return a tags body: {}",
        out.body
    );
    assert_ne!(
        out.body.get("source").and_then(Value::as_str),
        Some("unavailable"),
        "400 {what} must not look like tags_for unavailable: {}",
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        !err.contains("unknown provider"),
        "400 {what} must not be unknown provider: {}",
        out.body
    );
}

fn assert_local_models_not_a_member(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} must be 403 not-a-member: {}",
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("not a member"),
        "{what} 403 should say not a member: {}",
        out.body
    );
    assert!(
        out.body.get("models").is_none(),
        "{what} must not return a catalog body: {}",
        out.body
    );
    assert!(
        out.body.get("tags").and_then(Value::as_array).is_none(),
        "{what} must not return a tags body: {}",
        out.body
    );
    assert!(
        out.body.get("source").is_none(),
        "{what} must not return catalog/tags source: {}",
        out.body
    );
}

fn tcp_rows_to_remote_port(port: u16) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for path in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 10 {
                continue;
            }
            let Some((_, rem_port)) = cols[2].rsplit_once(':') else {
                continue;
            };
            let Ok(p) = u16::from_str_radix(rem_port, 16) else {
                continue;
            };
            if p == port {
                out.insert(format!("{}-{}-{}", cols[1], cols[2], cols[9]));
            }
        }
    }
    out
}

/// Reject-path GETs must not dial ollama.com (443) or the lab proxy (:1234).
struct LocalModelTagsNoOutbound {
    hits: std::sync::Arc<std::sync::atomic::AtomicU16>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    before_1234: std::collections::HashSet<String>,
    before_443: std::collections::HashSet<String>,
    joins: Vec<std::thread::JoinHandle<()>>,
}

impl LocalModelTagsNoOutbound {
    fn arm() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
        use std::sync::Arc;

        let before_1234 = tcp_rows_to_remote_port(1234);
        let before_443 = tcp_rows_to_remote_port(443);
        let hits = Arc::new(AtomicU16::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut joins = Vec::new();

        for addr in ["127.0.0.1:1234", "[::1]:1234"] {
            let Ok(listener) = std::net::TcpListener::bind(addr) else {
                continue;
            };
            let _ = listener.set_nonblocking(true);
            let hits = hits.clone();
            let stop = stop.clone();
            joins.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok(_) => hits.store(1234, Ordering::SeqCst),
                        Err(_) => std::thread::yield_now(),
                    }
                }
            }));
        }

        let hits_p = hits.clone();
        let stop_p = stop.clone();
        let before_1234_p = before_1234.clone();
        let before_443_p = before_443.clone();
        joins.push(std::thread::spawn(move || {
            while !stop_p.load(Ordering::Relaxed) {
                for p in [1234_u16, 443] {
                    let before = if p == 1234 {
                        &before_1234_p
                    } else {
                        &before_443_p
                    };
                    if tcp_rows_to_remote_port(p)
                        .difference(before)
                        .next()
                        .is_some()
                    {
                        hits_p.store(p, Ordering::SeqCst);
                    }
                }
                std::thread::yield_now();
            }
        }));

        Self {
            hits,
            stop,
            before_1234,
            before_443,
            joins,
        }
    }

    fn assert_idle(&self) {
        let port = self.hits.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            port, 0,
            "unsafe / other-ws / unauth local-model tags must not open the network (connect to :{port} is a fail)"
        );
        for (port, before) in [(1234_u16, &self.before_1234), (443, &self.before_443)] {
            let new_rows: Vec<_> = tcp_rows_to_remote_port(port)
                .difference(before)
                .cloned()
                .collect();
            assert!(
                new_rows.is_empty(),
                "must not open :{port} (ollama.com or lab proxy); new tcp rows {new_rows:?}"
            );
        }
    }
}

impl Drop for LocalModelTagsNoOutbound {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        for h in self.joins.drain(..) {
            let _ = h.join();
        }
    }
}

#[tokio::test]
async fn local_model_tags_rejects_unsafe_slug_other_ws_is_403() {
    let (app, _) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_other_email, other_jar, _other_ws) = signup_and_login(&app).await;
    let catalog = local_models_catalog_path(&ws);
    let safe_tags = local_model_tags_path(&ws, "llama3.2");
    let oversize: String = std::iter::repeat('x').take(129).collect();
    assert_eq!(oversize.len(), 129, "oversize slug must be 129 chars");

    // Slash / `..` must be percent-encoded so `{name}` is the unsafe slug
    // (a raw extra path segment would 404 before the HTTP gate).
    let rejects: Vec<(&str, String)> = vec![
        ("hi;rm raw", format!("{catalog}/hi;rm/tags")),
        ("hi;rm", local_model_tags_path(&ws, "hi;rm")),
        (
            "llama3.2/tags slash",
            local_model_tags_path(&ws, "llama3.2/tags"),
        ),
        ("../etc", local_model_tags_path(&ws, "../etc")),
        ("empty-after-trim", local_model_tags_path(&ws, " ")),
        ("129-char", local_model_tags_path(&ws, &oversize)),
    ];

    let guard = LocalModelTagsNoOutbound::arm();
    const FAIL_CLOSED: Duration = Duration::from_secs(2);
    let started = std::time::Instant::now();

    let anon_catalog = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", &catalog, None, None))
        .await
        .expect("unauth catalog must 401 immediately, not open ollama.com/:1234");
    assert_eq!(
        anon_catalog.status,
        StatusCode::UNAUTHORIZED,
        "unauth GET /local-models must stay 401: {}",
        anon_catalog.body
    );
    let anon_tags = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", &safe_tags, None, None))
        .await
        .expect("unauth tags must 401 immediately, not open ollama.com/:1234");
    assert_eq!(
        anon_tags.status,
        StatusCode::UNAUTHORIZED,
        "unauth GET /local-models/llama3.2/tags must be 401: {}",
        anon_tags.body
    );

    for (label, path) in &rejects {
        for pass in ["first", "after reject"] {
            let out = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", path, Some(&jar), None))
                .await
                .unwrap_or_else(|_| {
                    panic!("{label} {pass} must 400 immediately, not open ollama.com/:1234")
                });
            assert_local_model_tags_400(&out, &format!("{label} {pass} {path}"));
        }
    }

    let other_catalog = tokio::time::timeout(
        FAIL_CLOSED,
        call(&app, "GET", &catalog, Some(&other_jar), None),
    )
    .await
    .expect("other-ws catalog must 403 immediately, not open ollama.com/:1234");
    assert_local_models_not_a_member(&other_catalog, "other-ws GET /local-models");
    let other_tags = tokio::time::timeout(
        FAIL_CLOSED,
        call(&app, "GET", &safe_tags, Some(&other_jar), None),
    )
    .await
    .expect("other-ws tags must 403 immediately, not open ollama.com/:1234");
    assert_local_models_not_a_member(&other_tags, "other-ws GET /local-models/llama3.2/tags");

    assert!(
        started.elapsed() < FAIL_CLOSED,
        "unsafe / other-ws / unauth local-model tags must fail closed without a live fetch, took {:?}",
        started.elapsed()
    );
    guard.assert_idle();
    drop(guard);

    // Control: do not require a live ollama.com fetch.
    let control = call(&app, "GET", &safe_tags, Some(&jar), None).await;
    let err = control.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        !err.contains("unknown provider"),
        "safe slug llama3.2 must not 400 as unknown provider: {}",
        control.body
    );
    assert_ne!(
        control.status,
        StatusCode::BAD_REQUEST,
        "safe slug llama3.2 must not 400 as unknown provider: {}",
        control.body
    );
    assert_eq!(
        control.status,
        StatusCode::OK,
        "safe slug llama3.2 may 200 (cache | ollama.com/library/llama3.2/tags | unavailable): {}",
        control.body
    );
    assert_eq!(
        control.body["name"].as_str(),
        Some("llama3.2"),
        "safe tags body name: {}",
        control.body
    );
    let source = control.body["source"].as_str().unwrap_or("");
    assert!(
        source == "cache"
            || source == "ollama.com/library/llama3.2/tags"
            || source == "unavailable",
        "safe slug source must be cache | ollama.com/library/llama3.2/tags | unavailable, got {source:?}: {}",
        control.body
    );
    assert!(
        control.body.get("tags").and_then(Value::as_array).is_some(),
        "safe slug 200 must be a tags body: {}",
        control.body
    );
}

/// catalog:list-max-job-timeout-below-60-400 — PATCH max_job_timeout_s
/// below 60 is 400 (not clamp-and-200, not a SQL CHECK 500). GET after
/// each reject still has the previous value (default 3600). PATCH 60 may
/// persist. Same-ws member and other-ws must not write. Plane id must
/// not invent a worker_hosts row to store timeout.
/// Distinct from list-max-jobs-clamp-1-256 (`max_concurrent` 1–256) and
/// list-paused-host-lease-null. spawn `job_timeout_s` units already exist
/// — they are not this HTTP lock. Product failure: a 0s cap is stored.
fn assert_max_job_timeout_range_400(out: &HttpOut, raw: i32) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "max_job_timeout_s={raw} must not 500 from the SQL CHECK: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "max_job_timeout_s={raw} must not clamp-and-200: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "max_job_timeout_s={raw} must be 400: {}",
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("max_job_timeout"),
        "400 body must name max_job_timeout_s: {}",
        out.body
    );
    assert!(
        out.body.get("max_job_timeout_s").is_none() || out.body["max_job_timeout_s"] != raw,
        "400 body must not persist a {raw}s cap: {}",
        out.body
    );
}

async fn assert_host_max_job_timeout_s(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    want: i32,
    what: &str,
) {
    let got = call(app, "GET", path, Some(jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{what} GET: {}", got.body);
    assert_eq!(
        got.body["max_job_timeout_s"].as_i64(),
        Some(i64::from(want)),
        "{what} must leave stored max_job_timeout_s={want}: {}",
        got.body
    );
}

fn assert_timeout_patch_did_not_write(out: &HttpOut, raw: i32, what: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} must not 200-write max_job_timeout_s={raw}: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{what} must not 500: {}",
        out.body
    );
    assert!(
        out.body.get("max_job_timeout_s").is_none()
            || out.body["max_job_timeout_s"].as_i64() != Some(i64::from(raw)),
        "{what} must not look applied: {}",
        out.body
    );
}

#[tokio::test]
async fn max_job_timeout_below_60_is_400_and_stored_value_unchanged() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let plane_id = fleet_db::control_plane_runner_id(ws_id);
    let name = format!("timeout-{}", Uuid::new_v4());
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    let hid = host_id.to_string();
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    assert_eq!(
        seed.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "paired host default max_job_timeout_s is 3600: {}",
        seed.body
    );
    let max_concurrent0 = seed.body["max_concurrent"].clone();
    let paused0 = seed.body["paused"].clone();

    // README / schema CHECK: int ≥ 60. 0, 59, and negative must 400 —
    // not clamp-and-200, not a DB 500 that leaves a 0s cap stored.
    for raw in [0, 59, -1] {
        let rejected = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "max_job_timeout_s": raw })),
        )
        .await;
        assert_max_job_timeout_range_400(&rejected, raw);
        assert_host_max_job_timeout_s(
            &app,
            &jar,
            &host_path,
            3600,
            &format!("after owner reject {raw}"),
        )
        .await;
    }

    let still_seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        still_seed.status,
        StatusCode::OK,
        "GET after below-60 rejects: {}",
        still_seed.body
    );
    assert_eq!(
        still_seed.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "GET after reject still default 3600 (0s cap must not persist): {}",
        still_seed.body
    );
    assert_eq!(
        still_seed.body["max_concurrent"], max_concurrent0,
        "timeout rejects must not write max_concurrent (list-max-jobs-clamp-1-256): {}",
        still_seed.body
    );
    assert_eq!(
        still_seed.body["paused"], paused0,
        "timeout rejects must not write paused (list-paused-host-lease-null): {}",
        still_seed.body
    );

    let persist = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_job_timeout_s": 60 })),
    )
    .await;
    assert_eq!(
        persist.status,
        StatusCode::OK,
        "PATCH 60 may persist: {}",
        persist.body
    );
    assert_eq!(
        persist.body["max_job_timeout_s"].as_i64(),
        Some(60),
        "PATCH 60 must store 60 (not stay at 3600): {}",
        persist.body
    );
    assert_host_max_job_timeout_s(&app, &jar, &host_path, 60, "GET after persist 60").await;

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let listed_host = listed.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("list missing host: {}", listed.body));
    assert_eq!(
        listed_host["max_job_timeout_s"].as_i64(),
        Some(60),
        "list must persist 60: {}",
        listed.body
    );

    // After a real persist, another below-60 reject must keep 60, not
    // snap back to the create default and not store the 0s cap.
    for raw in [0, 59, -1] {
        let rejected = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "max_job_timeout_s": raw })),
        )
        .await;
        assert_max_job_timeout_range_400(&rejected, raw);
        assert_host_max_job_timeout_s(
            &app,
            &jar,
            &host_path,
            60,
            &format!("after reject {raw} following persist 60"),
        )
        .await;
    }

    let member_email = unique_email();
    let signed = signup(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        signed.status,
        StatusCode::CREATED,
        "member signup: {}",
        signed.body
    );
    let (member_login, member_jar) = login(&app, &member_email, TEST_PASSWORD).await;
    assert_eq!(
        member_login.status,
        StatusCode::OK,
        "member login: {}",
        member_login.body
    );
    let me = call(&app, "GET", "/api/auth/me", Some(&member_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "member me: {}", me.body);
    let member_id =
        Uuid::parse_str(me.body["user"]["id"].as_str().expect("user.id")).expect("uuid");
    let mut tx = state.db.begin().await.expect("begin");
    fleet_db::repos::workspaces::add_member_tx(
        &mut tx,
        ws_id,
        member_id,
        "member",
        chrono::Utc::now(),
    )
    .await
    .expect("add member");
    tx.commit().await.expect("commit member");

    let member_patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&member_jar),
        Some(json!({ "max_job_timeout_s": 60 })),
    )
    .await;
    assert_eq!(
        member_patch.status,
        StatusCode::FORBIDDEN,
        "same-ws member PATCH 60 must 403 (not write): {}",
        member_patch.body
    );
    assert_timeout_patch_did_not_write(&member_patch, 60, "member PATCH 60");

    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    assert_ne!(ws, other_ws, "A and B must have distinct workspaces");
    let other_on_a = call(
        &app,
        "PATCH",
        &host_path,
        Some(&other_jar),
        Some(json!({ "max_job_timeout_s": 60 })),
    )
    .await;
    assert!(
        other_on_a.status == StatusCode::FORBIDDEN || other_on_a.status == StatusCode::NOT_FOUND,
        "other-ws cookie PATCH on A's hid must be 403/404 (not write): {} {}",
        other_on_a.status,
        other_on_a.body
    );
    assert_timeout_patch_did_not_write(&other_on_a, 60, "other-ws on A's path");

    let other_on_own_path = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&other_ws)),
        Some(&other_jar),
        Some(json!({ "max_job_timeout_s": 60 })),
    )
    .await;
    assert!(
        other_on_own_path.status == StatusCode::FORBIDDEN
            || other_on_own_path.status == StatusCode::NOT_FOUND,
        "other-ws PATCH A's hid on B's path must be 403/404 (not write): {} {}",
        other_on_own_path.status,
        other_on_own_path.body
    );
    assert_timeout_patch_did_not_write(&other_on_own_path, 60, "other-ws on B's path");
    assert_host_max_job_timeout_s(&app, &jar, &host_path, 60, "after member/other-ws 403").await;

    let restore = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_job_timeout_s": 3600 })),
    )
    .await;
    assert_eq!(
        restore.status,
        StatusCode::OK,
        "optional PATCH 3600 restores default: {}",
        restore.body
    );
    assert_eq!(
        restore.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "PATCH 3600 must restore default: {}",
        restore.body
    );
    assert_host_max_job_timeout_s(&app, &jar, &host_path, 3600, "GET after restore 3600").await;
    assert_not_a_hosts_row(&state, ws_id, plane_id).await;
}
