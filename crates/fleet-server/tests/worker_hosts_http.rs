//! HTTP tests for paired worker hosts and public install scripts.
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use fleet_auth::{ACCESS_COOKIE, AuthConfig, REFRESH_COOKIE};
use fleet_db::{PoolConfig, connect_and_migrate_with};
use fleet_integrations::{CredentialsKey, IntegrationsConfig};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{
    AppState, ClaudeConfig, GeminiConfig, GrokConfig, OllamaConfig, OpenaiConfig, ServerConfig,
    build_http_app,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_PASSWORD: &str = "correct-horse";
const TEST_ADMIN_TOKEN: &str = "test-admin";
const TEST_JWT_SECRET: &str = "dev-change-me-to-a-long-random-secret-at-least-32-chars";
const TEST_BOOTSTRAP_SECRET: &str = "test-bootstrap-secret-24ch";

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
        instance_admin_emails: Vec::new(),
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
        gitea: None,
        gitea_base_url: "http://localhost:3033".into(),
        gogs_base_url: "http://localhost:3034".into(),
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
        bitbucket: None,
        confluence: None,
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
        gemini: GeminiConfig {
            enabled: true,
            api_key: None,
            api_base: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
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
        gemini: config.gemini.clone(),
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

/// catalog:list-worker-hosts-list-other-ws-403 — B GET /worker-hosts on
/// A's path is 403 and must not leak planted inventory. GET detail is
/// host_foreign_id. B-own-list is mutate + repo. Cookie-less GET is
/// list-unauthenticated-list-get-401. Cluster list merge is
/// list-cluster-host-not-injected.
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

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let out = call(&app, "GET", &hosts_path(&ws_a), Some(&jar_b), None).await;
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "B GET /worker-hosts on A's path: {}",
        out.raw
    );
    assert!(
        out.body.get("hosts").is_none(),
        "B 403 must not include a hosts key: {}",
        out.body
    );
    for (label, needle) in [
        ("host id", hid.as_str()),
        ("name", a_name.as_str()),
        ("hardware", hardware_os.as_str()),
        ("software", software_sha.as_str()),
        ("machine_token", a_machine.as_str()),
    ] {
        assert!(
            !needle.is_empty()
                && !out.raw.contains(needle)
                && !json_contains_str(&out.body, needle),
            "B GET list 403 must not contain A's {label} ({needle}): {}",
            out.raw
        );
    }
    assert!(
        !out.raw.contains("flm_") && !json_contains_str(&out.body, "flm_"),
        "B GET list 403 must not contain flm_: {}",
        out.raw
    );
}

/// catalog:list-unauthenticated-list-get-401 — cookie-less GET
/// /worker-hosts and GET /worker-hosts/{hid} on a real workspace are
/// 401 and must not leak hosts/inventory. Create is
/// unauthenticated_create_is_401. Authenticated foreign list is
/// list-worker-hosts-list-other-ws-403.
#[tokio::test]
async fn unauthenticated_list_and_get_are_401() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("anon-list-{}", Uuid::new_v4());
    let (hid, machine) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    let hid = hid.to_string();
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );

    let stamp = Uuid::new_v4();
    let hardware_os = format!("anon-hw-os-{stamp}");
    let software_sha = format!("anon-sw-sha-{stamp}");
    let inventory = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
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
        "inventory heartbeat: {}",
        inventory.body
    );

    let list = call(&app, "GET", &hosts_path(&ws), None, None).await;
    assert_eq!(
        list.status,
        StatusCode::UNAUTHORIZED,
        "cookie-less GET /worker-hosts: {}",
        list.raw
    );
    assert!(
        list.body.get("hosts").is_none(),
        "cookie-less list 401 must not include a hosts key: {}",
        list.body
    );
    for (label, needle) in [
        ("host id", hid.as_str()),
        ("name", name.as_str()),
        ("hardware", hardware_os.as_str()),
        ("software", software_sha.as_str()),
        ("machine_token", machine.as_str()),
    ] {
        assert!(
            !needle.is_empty()
                && !list.raw.contains(needle)
                && !json_contains_str(&list.body, needle),
            "cookie-less list 401 must not contain {label} ({needle}): {}",
            list.raw
        );
    }

    let detail = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        None,
        None,
    )
    .await;
    assert_eq!(
        detail.status,
        StatusCode::UNAUTHORIZED,
        "cookie-less GET /worker-hosts/{{hid}}: {}",
        detail.raw
    );
    assert!(
        detail.body.get("hosts").is_none(),
        "cookie-less detail 401 must not include a hosts key: {}",
        detail.body
    );
    for (label, needle) in [
        ("host id", hid.as_str()),
        ("name", name.as_str()),
        ("hardware", hardware_os.as_str()),
        ("software", software_sha.as_str()),
        ("machine_token", machine.as_str()),
    ] {
        assert!(
            !needle.is_empty()
                && !detail.raw.contains(needle)
                && !json_contains_str(&detail.body, needle),
            "cookie-less detail 401 must not contain {label} ({needle}): {}",
            detail.raw
        );
    }
}

/// catalog:list-cluster-host-not-injected — bootstrap cluster on A;
/// B GET /worker-hosts on B must not contain that hid. http_list does
/// not merge other workspaces' cluster hosts. Mutate is
/// list-cluster-host-foreign-ws-cannot-mutate. SQL filter is
/// list_for_workspace_omits_other_workspace_cluster_host.
#[tokio::test]
async fn cluster_host_not_injected_into_other_workspace_list() {
    let app = app().await;
    let (_email_a, _jar_a, ws_a) = signup_and_login(&app).await;
    let hostname = format!("cluster-list-{}", Uuid::new_v4());

    let boot_secret = TEST_BOOTSTRAP_SECRET.to_string();
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
    let hid = boot.body["host_id"]
        .as_str()
        .unwrap_or_else(|| panic!("bootstrap host_id: {}", boot.body))
        .to_string();
    let boot_labels = boot.body["cp_settings"]["labels"]
        .as_array()
        .unwrap_or_else(|| panic!("bootstrap cp_settings.labels: {}", boot.body));
    assert!(
        boot_labels.iter().any(|l| l
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("cluster"))),
        "bootstrap must stamp labels∋cluster: {}",
        boot.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let out = call(&app, "GET", &hosts_path(&ws_b), Some(&jar_b), None).await;
    assert_eq!(
        out.status,
        StatusCode::OK,
        "B GET /worker-hosts on B: {}",
        out.body
    );
    let hosts = out.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("B list hosts array: {}", out.body));
    assert!(
        hosts.iter().all(|h| h["id"].as_str() != Some(hid.as_str())),
        "B GET list must not include A's cluster hid {hid}: {}",
        out.body
    );
    assert!(
        !out.raw.contains(&hid) && !json_contains_str(&out.body, &hid),
        "B GET list must not contain A's cluster hid {hid}: {}",
        out.raw
    );
}

/// catalog:list-worker-hosts-mutate-other-ws-403 — B POST pair-tokens
/// on A's path is 403 not-a-member and must not mint. Owner rotate
/// still issues a new flp_. POST create is host_create_name; DELETE is
/// host_foreign_id. GET list is list-worker-hosts-list-other-ws-403.
/// member_can_list / admin_cannot_mutate are same-ws owner-required.
/// B POST pair-tokens of A's hid on B's path is
/// list-pair-tokens-foreign-id-404. Cookie-less is
/// list-unauthenticated-pair-tokens-delete-401.
fn assert_other_ws_host_mutate_is_403_and_does_not_mint(out: &HttpOut, when: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{when} must not 200 pair: {}",
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
    let app = app().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());

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
    let a_pair = created.body["pair_token"]
        .as_str()
        .expect("A create pair_token")
        .to_string();
    assert!(
        a_pair.starts_with("flp_"),
        "A create pair_token prefix: {a_pair}"
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

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
        "A POST pair-tokens as A after B 403: {}",
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
}

/// catalog:list-pair-tokens-foreign-id-404 — B POST pair-tokens of A's
/// hid on B's path is 404 (require_mutable_host) and must not mint flp_.
/// host_foreign_id is GET/PATCH/DELETE only. B on A's path is
/// list-worker-hosts-mutate-other-ws-403.
#[tokio::test]
async fn worker_hosts_pair_tokens_foreign_id_on_own_path_is_404_and_does_not_mint() {
    let app = app().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());
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
    let a_pair = created.body["pair_token"]
        .as_str()
        .expect("A create pair_token")
        .to_string();
    assert!(
        a_pair.starts_with("flp_"),
        "A create pair_token prefix: {a_pair}"
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let out = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws_b)),
        Some(&jar_b),
        Some(json!({})),
    )
    .await;
    assert_ne!(
        out.status,
        StatusCode::OK,
        "B POST pair-tokens of A's hid on B's path must not 200: {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "B POST pair-tokens of A's hid on B's path must be 404: {}",
        out.raw
    );
    for key in ["pair_token", "machine_token", "install_hint"] {
        assert!(
            !json_has_key(&out.body, key),
            "B 404 must not include a new {key}: {}",
            out.body
        );
    }
    assert!(
        !out.raw.contains("flp_") && !json_contains_str(&out.body, "flp_"),
        "B 404 must not mint pair_token (flp_): {}",
        out.raw
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
        "A POST pair-tokens as A after B 404: {}",
        minted.body
    );
    let flp = minted.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("A pair-tokens must mint a new flp_: {}", minted.body));
    assert!(flp.starts_with("flp_"), "A mint prefix: {flp}");
    assert_ne!(
        flp, a_pair,
        "A rotate must issue a new flp_ (B must not have minted on B's path): {}",
        minted.body
    );
}

/// catalog:list-unauthenticated-pair-tokens-delete-401 — cookie-less
/// POST pair-tokens and DELETE on a real hid are 401. Create is
/// unauthenticated_create_is_401. Authenticated foreign is
/// list-worker-hosts-mutate-other-ws-403 / list-pair-tokens-foreign-id-404.
#[tokio::test]
async fn unauthenticated_pair_tokens_and_delete_are_401() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("anon-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "setup POST /worker-hosts: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let rotate = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotate.status,
        StatusCode::UNAUTHORIZED,
        "cookie-less POST pair-tokens: {}",
        rotate.body
    );
    assert!(
        !rotate.raw.contains("flp_") && !json_contains_str(&rotate.body, "flp_"),
        "cookie-less pair-tokens 401 must not mint flp_: {}",
        rotate.raw
    );

    let del = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        None,
        None,
    )
    .await;
    assert_eq!(
        del.status,
        StatusCode::UNAUTHORIZED,
        "cookie-less DELETE: {}",
        del.body
    );

    let leftover = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "owner GET after cookie-less 401s must still 200: {}",
        leftover.body
    );
    assert_eq!(leftover.body["id"].as_str(), Some(hid.as_str()));
}

/// catalog:list-this-machine-only-on-loopback-origin — GET /worker-hosts
/// `is_this_machine` is plane_box && viewer_loopback. Pair hostname to
/// local_hostname so the card can be true; LAN / substring Origin stay false.
#[tokio::test]
async fn this_machine_origin_is_true_only_for_real_loopback() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);
    let hid = pair_plane_box_host(&app, &jar, &ws).await;

    let loopback = call_with_headers(
        &app,
        "GET",
        &uri,
        Some(&jar),
        &[("origin", "http://localhost:3030")],
        None,
    )
    .await;
    assert_eq!(
        loopback.status,
        StatusCode::OK,
        "loopback Origin GET /worker-hosts: {}",
        loopback.body
    );
    assert_plane_this_machine(
        list_host_by_id(&loopback, &hid, "loopback Origin"),
        true,
        "Origin=http://localhost:3030",
    );

    for origin in ["http://127.0.0.1:3030", "http://[::1]:3030"] {
        let list =
            call_with_headers(&app, "GET", &uri, Some(&jar), &[("origin", origin)], None).await;
        assert_eq!(
            list.status,
            StatusCode::OK,
            "loopback Origin {origin} GET /worker-hosts: {}",
            list.body
        );
        assert_plane_this_machine(list_host_by_id(&list, &hid, origin), true, origin);
    }

    let lan = call_with_headers(
        &app,
        "GET",
        &uri,
        Some(&jar),
        &[("origin", "http://192.168.1.50:3030")],
        None,
    )
    .await;
    assert_eq!(
        lan.status,
        StatusCode::OK,
        "LAN Origin GET /worker-hosts: {}",
        lan.body
    );
    assert_plane_this_machine(
        list_host_by_id(&lan, &hid, "LAN Origin"),
        false,
        "Origin=http://192.168.1.50:3030",
    );

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
        assert_plane_this_machine(list_host_by_id(&list, &hid, &label), false, &label);
    }
}

/// Referer-only loopback still tags the paired plane card; no Origin/Referer does not.
#[tokio::test]
async fn paired_plane_host_referer_only_loopback_still_tags() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);
    let hid = pair_plane_box_host(&app, &jar, &ws).await;

    let referer = call_with_headers(
        &app,
        "GET",
        &uri,
        Some(&jar),
        &[("referer", "http://[::1]:3030")],
        None,
    )
    .await;
    assert_eq!(
        referer.status,
        StatusCode::OK,
        "Referer [::1] GET /worker-hosts: {}",
        referer.body
    );
    assert_plane_this_machine(
        list_host_by_id(&referer, &hid, "Referer [::1]"),
        true,
        "Referer=http://[::1]:3030",
    );

    let no_origin = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        no_origin.status,
        StatusCode::OK,
        "no Origin GET /worker-hosts: {}",
        no_origin.body
    );
    assert_plane_this_machine(
        list_host_by_id(&no_origin, &hid, "no Origin"),
        false,
        "no Origin/Referer",
    );
}

/// Same resolution as fleet-server `local_hostname` (HOSTNAME, /etc/hostname,
/// `uname -n`, `hostname`).
fn plane_local_hostname() -> String {
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
        .or_else(|| command_stdout("uname", &["-n"]))
        .or_else(|| command_stdout("hostname", &[]))
        .unwrap_or_default()
}

fn command_stdout(bin: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(bin).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

async fn pair_plane_box_host(app: &axum::Router, jar: &CookieJar, ws: &str) -> String {
    let local = plane_local_hostname();
    assert!(
        !local.is_empty(),
        "a local hostname is required to pair a plane-box host"
    );
    let name = format!("plane-{}", Uuid::new_v4());
    let created = call(
        app,
        "POST",
        &hosts_path(ws),
        Some(jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let pair_token = created.body["pair_token"].as_str().expect("pair_token");
    let paired = call(
        app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(pair_token, &local)),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "pair hostname={local}: {}",
        paired.body
    );
    hid
}

fn assert_plane_this_machine(card: &Value, want_this_machine: bool, what: &str) {
    assert_eq!(
        card.get("is_control_plane_box").and_then(Value::as_bool),
        Some(true),
        "{what} is_control_plane_box must stay true: {card}"
    );
    assert_eq!(
        card.get("is_this_machine").and_then(Value::as_bool),
        Some(want_this_machine),
        "{what} is_this_machine want {want_this_machine}: {card}"
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

/// A pending unpaired host has no hostname. Loopback Origin must not
/// invent a synthetic plane card and tag that instead.
#[tokio::test]
async fn pending_host_is_not_this_machine_on_loopback_origin() {
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
}

/// catalog:list-host-create-name — POST empty / whitespace / omitted name
/// is 200: name is optional; pair stamps the device hostname. name.len()
/// 256 and 10k are 400, not clamp-and-200. Add worker still sends a
/// default UI name. Exactly 255 is host_create_name_exactly_255. PATCH
/// still rejects empty (list-rename-host-patch).
#[tokio::test]
async fn host_create_name_empty_ok_huge_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);

    let snapshot = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        snapshot.status,
        StatusCode::OK,
        "pre-call GET /worker-hosts: {}",
        snapshot.body
    );
    let snap_hosts = snapshot.body["hosts"].as_array().expect("hosts array");
    let snap_count = snap_hosts.len();

    for (label, body) in [
        ("empty", json!({ "name": "" })),
        ("whitespace", json!({ "name": "   " })),
        ("omitted", json!({})),
    ] {
        let out = call(&app, "POST", &uri, Some(&jar), Some(body)).await;
        assert_eq!(
            out.status,
            StatusCode::OK,
            "POST {label} name is optional (hostname on pair): {}",
            out.body
        );
        assert!(
            out.raw.contains("flp_"),
            "accepted {label} create must mint a pair token: {}",
            out.raw
        );
        assert_eq!(
            out.body["host"]["name"].as_str(),
            Some(""),
            "POST {label} pending name stays empty until pair: {}",
            out.body
        );
    }

    let after_ok = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(after_ok.status, StatusCode::OK, "{}", after_ok.body);
    let after_ok_hosts = after_ok.body["hosts"].as_array().expect("hosts array");
    assert_eq!(
        after_ok_hosts.len(),
        snap_count + 3,
        "empty/whitespace/omitted each insert one pending row: {}",
        after_ok.body
    );

    let name_256 = "x".repeat(256);
    let name_10k = "x".repeat(10_000);
    let after_ok_ids: Vec<String> = after_ok_hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    for (label, body) in [
        ("256", json!({ "name": name_256 })),
        ("10k", json!({ "name": name_10k })),
    ] {
        let out = call(&app, "POST", &uri, Some(&jar), Some(body)).await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "POST {label} name must be 400, not clamp-and-200: {}",
            out.body
        );
        assert!(
            !out.raw.contains("flp_"),
            "rejected {label} create must not leak a pair token: {}",
            out.raw
        );

        let after = call(&app, "GET", &uri, Some(&jar), None).await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "GET after {label} reject: {}",
            after.body
        );
        let hosts = after.body["hosts"].as_array().expect("hosts array");
        let ids: Vec<String> = hosts
            .iter()
            .filter_map(|h| h["id"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            hosts.len(),
            after_ok_hosts.len(),
            "POST {label} name must not add a GET /worker-hosts row: {}",
            after.body
        );
        assert_eq!(
            ids, after_ok_ids,
            "POST {label} name must not introduce a new host id: {ids:?} vs {after_ok_ids:?}"
        );
        assert!(
            !after.raw.contains(&name_256) && !after.raw.contains(&name_10k),
            "GET after {label} reject must not persist a >255 name: {}",
            after.body
        );
    }
}

/// catalog:list-host-create-name-exactly-255 — POST unique name of 255
/// bytes is 200 and GET /worker-hosts lists that name. Empty name is
/// optional; 256 is host_create_name_empty_ok_huge_is_400. PATCH 255 is
/// list-rename-host-patch.
#[tokio::test]
async fn host_create_name_exactly_255_is_200_and_lists() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let uri = hosts_path(&ws);

    let stamp = Uuid::new_v4().to_string();
    let name = format!("{stamp}{}", "n".repeat(255 - stamp.len()));
    assert_eq!(name.len(), 255, "control name must be exactly 255 bytes");

    let out = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::OK,
        "POST unique 255-byte name must be 200: {}",
        out.body
    );
    assert_eq!(
        out.body["host"]["name"].as_str(),
        Some(name.as_str()),
        "create must keep the 255-byte name: {}",
        out.body
    );

    let listed = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "GET after 255-byte create: {}",
        listed.body
    );
    assert!(
        listed.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|h| h["name"].as_str() == Some(name.as_str())),
        "255-byte name must appear on GET /worker-hosts: {}",
        listed.body
    );
}

/// Cancel unused pending: owner DELETE is 200 disabled and the original
/// flp_ must not pair.
#[tokio::test]
async fn cancel_unused_pending_http_discards_never_paired_hidden_not_assignable() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("never-paired-{}", Uuid::new_v4());

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

    let pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": name,
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
}

/// After owner DELETE of unused pending, GET /worker-hosts/{hid} is 200
/// status=disabled paired_at=null.
#[tokio::test]
async fn cancel_unused_pending_get_detail_is_200_disabled() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("cancel-detail-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(|| panic!("host.id: {}", created.body));

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
        "DELETE unused pending: {}",
        revoked.body
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
        "GET after Cancel unused pending: {}",
        got.body
    );
    assert_eq!(
        got.body["status"], "disabled",
        "GET detail after unused DELETE must be disabled: {}",
        got.body
    );
    assert!(
        got.body["paired_at"].is_null(),
        "GET detail after unused DELETE stays never-paired: {}",
        got.body
    );
}

/// POST create `install_hint` and pair `control_plane_url` embed
/// WORKER_PUBLIC_BASE via `created_payload` / `paired_payload` /
/// `worker_api_base`, not API_PUBLIC_BASE.
#[tokio::test]
async fn install_hint_worker_public_base_not_api_public_base() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let _env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
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
        expected_worker_install_hint(WORKER, &pair_token),
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
        Some(pair_body(&pair_token, "studio")),
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
}

/// POST create `install_hint` falls back to API_PUBLIC_BASE when
/// WORKER_PUBLIC_BASE is unset at start (`worker_api_base` env miss).
#[tokio::test]
async fn create_install_hint_falls_back_to_api_when_worker_public_base_unset() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let _env = PublicBaseEnvGuard::lock_and_set(None, Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "unset-hint" })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create with WORKER_PUBLIC_BASE unset: {}",
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("unset pair_token: {}", created.body));
    let hint = created.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("unset install_hint: {}", created.body));
    assert_eq!(
        hint,
        expected_worker_install_hint(API, pair_token),
        "unset WORKER_PUBLIC_BASE install_hint falls back to API_PUBLIC_BASE"
    );
    assert!(
        !hint.contains(WORKER),
        "unset fallback install_hint must not embed WORKER_PUBLIC_BASE: {hint}"
    );
}

/// POST create `install_hint` falls back to API_PUBLIC_BASE when
/// WORKER_PUBLIC_BASE is blank/whitespace (`worker_api_base` trim/filter).
#[tokio::test]
async fn create_install_hint_falls_back_to_api_when_worker_public_base_blank() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let _env = PublicBaseEnvGuard::lock_and_set(Some("   "), Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "blank-hint" })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create with blank WORKER_PUBLIC_BASE: {}",
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("blank pair_token: {}", created.body));
    let hint = created.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("blank install_hint: {}", created.body));
    assert_eq!(
        hint,
        expected_worker_install_hint(API, pair_token),
        "blank WORKER_PUBLIC_BASE install_hint falls back to API_PUBLIC_BASE"
    );
    assert!(
        !hint.contains(WORKER),
        "blank fallback install_hint must not embed WORKER_PUBLIC_BASE: {hint}"
    );
}

/// POST …/worker-hosts/{id}/pair-tokens `install_hint` uses WORKER_PUBLIC_BASE
/// (`http_new_pair_token` → `created_payload`).
#[tokio::test]
async fn rotate_install_hint_uses_worker_public_base() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let _env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "rotate-hint" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("create host.id: {}", created.body))
        .to_string();
    let old_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();

    let rotated = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotated.status,
        StatusCode::OK,
        "POST pair-tokens: {}",
        rotated.body
    );
    let new_token = rotated.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("rotate pair_token: {}", rotated.body));
    assert_ne!(
        new_token, old_token,
        "rotate must mint a new pair token: {}",
        rotated.body
    );
    let hint = rotated.body["install_hint"]
        .as_str()
        .unwrap_or_else(|| panic!("rotate install_hint: {}", rotated.body));
    assert_eq!(
        hint,
        expected_worker_install_hint(WORKER, new_token),
        "rotate install_hint must come from created_payload/worker_api_base"
    );
    assert!(
        !hint.contains(API),
        "rotate install_hint must not embed API_PUBLIC_BASE: {hint}"
    );
    assert!(
        !rotated.raw.contains(API),
        "rotate body must not embed API_PUBLIC_BASE"
    );
}

/// IntegrationsConfig::from_env reads API_PUBLIC_BASE only; GET catalog
/// github.oauth_callback_url stays on that origin when WORKER is also set.
#[tokio::test]
async fn integrations_oauth_callback_url_from_env_ignores_worker_public_base() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);

    let _env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let from_env = IntegrationsConfig::from_env()
        .expect("IntegrationsConfig::from_env requires JWT_SECRET (same as boot)");
    assert_eq!(
        from_env.api_public_base, API,
        "from_env must read API_PUBLIC_BASE, not WORKER_PUBLIC_BASE"
    );
    assert_eq!(
        from_env.callback_url("github"),
        format!("{API}/api/integrations/oauth/callback/github")
    );
    assert!(
        !from_env.callback_url("github").contains(WORKER),
        "from_env OAuth callback must ignore WORKER_PUBLIC_BASE"
    );

    let (app, _) = app_with_state_opts(None, Some(from_env.api_public_base.as_str())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
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
}

/// catalog:list-pair-control-plane-url-ignores-host-header — pair
/// `control_plane_url` is WORKER_PUBLIC_BASE, never request Host /
/// X-Forwarded-Host. Distinct from
/// [`install_hint_worker_public_base_not_api_public_base`] (env preference,
/// no Host), [`pair_unset_worker_public_base_still_ignores_host_header`]
/// (API fallback under Host), [`bootstrap_ignores_host_header_for_control_plane_url`],
/// list-oauth-start-callback-ignores-host-header (Connect `redirect_uri`),
/// list-loopback-pair-warning-and-offbox, and
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
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("pair-host-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();

    let paired = call_with_headers(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        &pair_spoofed_host_headers(),
        Some(pair_body(
            &pair_token,
            &format!("pair-hn-{}", Uuid::new_v4()),
        )),
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
    assert_omits_evil_pair_origin(&paired.raw, "POST /pair with spoofed Host");
}

/// POST /pair with WORKER_PUBLIC_BASE unset still ignores Host /
/// X-Forwarded-Host: `control_plane_url` is API_PUBLIC_BASE.
#[tokio::test]
async fn pair_unset_worker_public_base_still_ignores_host_header() {
    const API: &str = "http://oauth.example.test:8080";
    assert!(
        !API.contains(PAIR_HOST_EVIL),
        "API_PUBLIC_BASE must not already be the spoofed Host"
    );

    let _env = PublicBaseEnvGuard::lock_and_set(None, Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("unset-host-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create with WORKER_PUBLIC_BASE unset: {}",
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("unset pair_token: {}", created.body))
        .to_string();

    let paired = call_with_headers(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        &pair_spoofed_host_headers(),
        Some(pair_body(
            &pair_token,
            &format!("unset-hn-{}", Uuid::new_v4()),
        )),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "pair with Host and WORKER unset: {}",
        paired.body
    );
    assert_eq!(
        paired.body["control_plane_url"].as_str(),
        Some(API),
        "unset WORKER_PUBLIC_BASE pair control_plane_url must be API_PUBLIC_BASE, not Host: {}",
        paired.body
    );
    assert_omits_evil_pair_origin(&paired.raw, "POST /pair with WORKER unset and spoofed Host");
}

/// POST /bootstrap with a seeded workspace uses WORKER_PUBLIC_BASE for
/// `control_plane_url`, never request Host / X-Forwarded-Host.
#[tokio::test]
async fn bootstrap_ignores_host_header_for_control_plane_url() {
    const WORKER: &str = "http://pair.example.test:9443";
    const API: &str = "http://oauth.example.test:8080";
    assert_ne!(WORKER, API);
    assert!(
        !WORKER.contains(PAIR_HOST_EVIL) && !API.contains(PAIR_HOST_EVIL),
        "configured bases must not already be the spoofed Host"
    );

    let _env = PublicBaseEnvGuard::lock_and_set(Some(WORKER), Some(API)).await;
    let (app, _) = app_with_state_opts(None, Some(API)).await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let boot = call_inner_headers(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        None,
        Some(TEST_BOOTSTRAP_SECRET),
        &pair_spoofed_host_headers(),
        Some(json!({
            "hostname": format!("boot-hn-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": ws,
        })),
    )
    .await;
    assert_eq!(
        boot.status,
        StatusCode::OK,
        "bootstrap with Host: {}",
        boot.body
    );
    assert_eq!(
        boot.body["control_plane_url"].as_str(),
        Some(WORKER),
        "bootstrap control_plane_url must be WORKER_PUBLIC_BASE, not Host: {}",
        boot.body
    );
    assert_omits_evil_pair_origin(&boot.raw, "POST /bootstrap with spoofed Host");
}

/// Pair token plaintext is returned on create. Later GET detail raw and
/// the list envelope must omit `pair_token` / `install_hint` / hash keys
/// and the snapshot flp_. Prefix/hint-contains is create/install_hint;
/// rotate mint is new_pair_token_only_while_pending.
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
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create must return pair_token: {}", created.body))
        .to_string();
    assert!(
        !pair_token.is_empty(),
        "create pair_token must be non-empty: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

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
    assert_http_omits_pair_plaintext(&got, &[&pair_token], "GET /worker-hosts/{id}");

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(list.status, StatusCode::OK, "list: {}", list.body);
    assert_http_omits_pair_plaintext(&list, &[&pair_token], "GET /worker-hosts");
}

/// After rotate, GET/list must omit both one-shot tokens. Mint/old-token
/// death is new_pair_token_old_token_dies.
#[tokio::test]
async fn pair_token_plaintext_omitted_from_get_and_list_after_rotate() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("rot-omit-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let old_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
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
    let new_token = rotated.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("rotate pair_token: {}", rotated.body))
        .to_string();

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after rotate: {}", got.body);
    assert_http_omits_pair_plaintext(
        &got,
        &[&old_token, &new_token],
        "GET /worker-hosts/{id} after rotate",
    );

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "list after rotate: {}",
        list.body
    );
    assert_http_omits_pair_plaintext(
        &list,
        &[&old_token, &new_token],
        "GET /worker-hosts after rotate",
    );
}

/// POST /pair and /bootstrap (paired_payload) must not contain
/// pair_token/install_hint keys or the consumed flp_.
#[tokio::test]
async fn pair_and_bootstrap_http_omit_consumed_flp_() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("omit-flp-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();
    let _hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, &name)),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    assert_http_omits_pair_plaintext(&paired, &[&pair_token], "POST /pair");

    let boot_secret = TEST_BOOTSTRAP_SECRET.to_string();
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
                "machine_id": _hid,
                "workspace_id": ws
            })),
        )
        .await
    };
    assert_eq!(
        boot.status,
        StatusCode::FORBIDDEN,
        "bootstrap must not rotate a paired host: {}",
        boot.body
    );
    let raw = boot.body.to_string();
    assert!(
        !raw.contains(&pair_token),
        "refused bootstrap must not echo the pair token: {raw}"
    );
}

/// After pair, POST /me/heartbeat and /me/lease wrap host_json plus extras
/// and must omit pair_token/install_hint keys and the used flp_.
#[tokio::test]
async fn heartbeat_and_lease_envelopes_omit_pair_token() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("omit-hb-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("create pair_token: {}", created.body))
        .to_string();

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, &name)),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let machine = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair machine_token: {}", paired.body))
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
    assert_http_omits_pair_plaintext(&hb, &[&pair_token], "POST /me/heartbeat");

    let lease = call_bearer(&app, "POST", "/api/worker-hosts/me/lease", &machine, None).await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert_http_omits_pair_plaintext(&lease, &[&pair_token], "POST /me/lease");
}

/// catalog:list-machine-token-plaintext-not-in-later-get — POST /pair
/// returns `machine_token` (`flm_…`) once. Later GET and /me/heartbeat
/// must omit that plaintext and have no `machine_token` /
/// `machine_token_hash` key. Cookie 401 is cookie_jwt; rotate identity
/// is bootstrap_pairs_and_rotates; after-rotate omit is
/// machine_token_plaintext_omitted_from_get_and_heartbeat_after_rotate.
#[tokio::test]
async fn machine_token_plaintext_only_on_pair_or_bootstrap_not_later_get_heartbeat_or_lease() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("mtok-{}", Uuid::new_v4());

    let (host_id, flm) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    assert!(
        flm.starts_with("flm_") && flm.len() > 8,
        "POST /pair must return machine_token flm_… once (control, not a tautology that every host JSON omits a token): {flm}"
    );
    let hid = host_id.to_string();

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
    assert_later_http_omits_machine_plaintext(&got, &[&flm], "GET /worker-hosts/{id} after pair");

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &flm,
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
        "heartbeat after pair: {}",
        hb.body
    );
    assert_later_http_omits_machine_plaintext(&hb, &[&flm], "POST /me/heartbeat after pair");
}

/// After bootstrap rotate, GET and /me/heartbeat must omit both one-shot
/// flm_ tokens. Mint/old-token death is bootstrap_pairs_and_rotates.
#[tokio::test]
async fn machine_token_plaintext_omitted_from_get_and_heartbeat_after_rotate() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("mtok-rot-{}", Uuid::new_v4());

    let (host_id, old_flm) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    assert!(
        old_flm.starts_with("flm_") && old_flm.len() > 8,
        "POST /pair must return machine_token flm_… once: {old_flm}"
    );
    let hid = host_id.to_string();

    let boot = {
        let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
        call_bearer(
            &app,
            "POST",
            "/api/worker-hosts/bootstrap",
            TEST_BOOTSTRAP_SECRET,
            Some(json!({
                "hostname": name,
                "platform": "linux-x86_64",
                "advertised_host": true,
                "advertised_container": false,
                "machine_id": hid,
                "workspace_id": ws
            })),
        )
        .await
    };
    assert_eq!(
        boot.status,
        StatusCode::FORBIDDEN,
        "bootstrap must not rotate a paired host: {}",
        boot.body
    );
    assert!(
        boot.body["machine_token"].is_null(),
        "refused bootstrap must not mint a token: {}",
        boot.body
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
        "get after refused rotate: {}",
        got.body
    );
    assert_later_http_omits_machine_plaintext(
        &got,
        &[&old_flm],
        "GET /worker-hosts/{id} after refused bootstrap",
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &old_flm,
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
        "original machine token still works: {}",
        hb.body
    );
    assert_later_http_omits_machine_plaintext(
        &hb,
        &[&old_flm],
        "POST /me/heartbeat after refused bootstrap",
    );
}

/// POST /me/heartbeat and /me/lease with a presented Bearer flm_ that is
/// not current must 401 and omit that plaintext in raw/body.
/// assert_later_http_omits_machine_plaintext requires 200.
#[tokio::test]
async fn unauthorized_machine_route_must_not_echo_presented_flm_() {
    let app = app().await;
    let presented = format!("flm_unauth-{}", Uuid::new_v4());
    assert!(
        presented.starts_with("flm_") && presented.len() > 8,
        "presented Bearer must be a searchable flm_: {presented}"
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &presented,
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
        StatusCode::UNAUTHORIZED,
        "garbage flm_ /me/heartbeat must 401: {}",
        hb.body
    );
    assert!(
        !hb.raw.contains(&presented) && !json_contains_str(&hb.body, &presented),
        "401 /me/heartbeat must not echo presented flm_: {}",
        hb.raw
    );

    let lease = call_bearer(&app, "POST", "/api/worker-hosts/me/lease", &presented, None).await;
    assert_eq!(
        lease.status,
        StatusCode::UNAUTHORIZED,
        "garbage flm_ /me/lease must 401: {}",
        lease.body
    );
    assert!(
        !lease.raw.contains(&presented) && !json_contains_str(&lease.body, &presented),
        "401 /me/lease must not echo presented flm_: {}",
        lease.raw
    );
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

/// catalog:list-host-foreign-id-404-omits-workspace — GET of another
/// workspace's host id on the caller's own path is 404 and must not
/// name that workspace. PATCH/DELETE on B's path and B on A's path
/// are mutate-other-ws / owner-rename siblings. POST pair-tokens on
/// B's path is list-pair-tokens-foreign-id-404. Cluster hosts are
/// list-cluster-host-label-undo /
/// list-patch-labels-cluster-not-cross-ws — this lock is a non-cluster pair.
#[tokio::test]
async fn host_foreign_id_get_on_caller_path_is_404_and_omits_workspace() {
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
    assert!(
        !labels_contain(&snap.body, "cluster"),
        "this lock is a non-cluster paired host: {}",
        snap.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");
    let b_path = format!("{}/{a_hid}", hosts_path(&ws_b));

    let out = call(&app, "GET", &b_path, Some(&jar_b), None).await;
    assert_ne!(
        out.status,
        StatusCode::OK,
        "B GET of A's hid on B's path must not return A's row: {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "B GET of A's hid on B's path must be 404: {}",
        out.raw
    );
    assert_eq!(
        out.body["error"].as_str(),
        Some("worker host not found"),
        "B GET 404 must use the generic not_found body: {}",
        out.body
    );
    assert_oneshot_raw_omits_a_host_tenant(
        &out,
        "B GET 404 on B's path",
        &ws_a,
        &a_name,
        &a_hostname,
        &a_machine,
    );

    let leftover = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "A GET as A after B GET on B's path must still 200: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["id"].as_str(),
        Some(a_hid.to_string().as_str())
    );
    assert_eq!(
        leftover.body["name"].as_str(),
        Some(a_name.as_str()),
        "B GET on B's path must not change A's name: {}",
        leftover.body
    );
}

/// Same cookie POSTs workspace B, then GET B/worker-hosts/{A-hid} is 404
/// and must not name A's host. Two-user host_foreign_id would stay green
/// under an "any host this user owns" rewrite.
#[tokio::test]
async fn host_same_user_other_workspace_hid_get_is_404() {
    let app = app().await;
    let (_email, jar, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-host-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar, &ws_a, &a_name, true, false).await;
    assert!(
        a_machine.starts_with("flm_"),
        "pair must mint a real machine token: {a_machine}"
    );

    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("other-ws-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "POST /api/workspaces B: {}",
        b_created.body
    );
    let ws_b = b_created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.id: {}", b_created.body))
        .to_string();
    assert_ne!(ws_a, ws_b, "A and B must be distinct workspaces");

    let out = call(
        &app,
        "GET",
        &format!("{}/{a_hid}", hosts_path(&ws_b)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "same-user GET of A's hid on B's path must be 404: {}",
        out.raw
    );
    assert_eq!(
        out.body["error"].as_str(),
        Some("worker host not found"),
        "same-user 404 must use the generic not_found body: {}",
        out.body
    );
    assert_oneshot_raw_omits_a_host_tenant(
        &out,
        "same-user GET 404 on B's path",
        &ws_a,
        &a_name,
        &a_name,
        &a_machine,
    );
}

/// Member GET /worker-hosts/{random-uuid} is 404 with the same generic
/// not_found body as the foreign-id case. list_has_no_synthetic_control_plane
/// only PATCH/DELETE the plane UUID; a 403-on-known vs 404-on-missing
/// oracle would stay green.
#[tokio::test]
async fn host_unknown_id_get_on_own_path_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let missing = Uuid::new_v4();

    let out = call(
        &app,
        "GET",
        &format!("{}/{missing}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_ne!(
        out.status,
        StatusCode::FORBIDDEN,
        "missing hid must not 403 (that would distinguish known vs missing): {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "GET /worker-hosts/{{random-uuid}} must be 404: {}",
        out.raw
    );
    assert_eq!(
        out.body["error"].as_str(),
        Some("worker host not found"),
        "unknown hid must use the same generic not_found as foreign hid: {}",
        out.body
    );
}

/// catalog:list-patch-labels-cluster-not-cross-ws — HTTP PATCH of cluster
/// spellings on a UI-paired laptop must not stamp the bootstrap token.
/// A GET omits cluster; B GET hid is 403/404; A's flm_ must not lease a
/// B-only spawn (`is_cluster_host` unscope).
#[tokio::test]
async fn patch_labels_cluster_must_not_make_paired_host_visible_to_other_workspace() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-laptop-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    let a_path = format!("{}/{a_hid}", hosts_path(&ws_a));

    let snap = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET before PATCH: {}",
        snap.body
    );
    assert!(
        labels_omit_cluster(&snap.body),
        "paired laptop must not start as cluster: {}",
        snap.body
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let patched = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["cluster"] })),
    )
    .await;
    assert!(
        patched.status == StatusCode::OK || patched.status == StatusCode::BAD_REQUEST,
        "PATCH labels=[cluster] must be 200 (strip) or 400 (reject): {} {}",
        patched.status,
        patched.body
    );

    let got = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "A GET after PATCH cluster: {}",
        got.body
    );
    assert!(
        labels_omit_cluster(&got.body),
        "PATCH must not stamp cluster on a paired laptop: {}",
        got.body
    );

    let b_get = call(
        &app,
        "GET",
        &format!("{}/{a_hid}", hosts_path(&ws_b)),
        Some(&jar_b),
        None,
    )
    .await;
    assert!(
        b_get.status == StatusCode::FORBIDDEN || b_get.status == StatusCode::NOT_FOUND,
        "B GET hid must be 403/404 after PATCH cluster: {} {}",
        b_get.status,
        b_get.body
    );

    let ws_b_id = Uuid::parse_str(&ws_b).expect("ws b");
    let _ = enqueue_spawn_for_host(&state, ws_b_id, a_hid).await;
    let a_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &a_machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&a_lease, "A's flm_ /me/lease after a B-only enqueue");
}

/// PATCH labels=["CLUSTER"] / [" Cluster "] must not stamp or unscope.
/// A filter of `l == "cluster"` stays green if this body only sent lowercase.
#[tokio::test]
async fn http_patch_CLUSTER_or_padded_cluster_must_not_unscope() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-laptop-{}", Uuid::new_v4());
    let (a_hid, a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    let a_path = format!("{}/{a_hid}", hosts_path(&ws_a));
    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["CLUSTER", " Cluster "] })),
    )
    .await;
    assert!(
        patched.status == StatusCode::OK || patched.status == StatusCode::BAD_REQUEST,
        "PATCH labels=[CLUSTER, padded] must be 200 or 400: {} {}",
        patched.status,
        patched.body
    );

    let got = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "A GET after PATCH CLUSTER/padded: {}",
        got.body
    );
    assert!(
        labels_omit_cluster(&got.body),
        "PATCH CLUSTER/padded must not stamp cluster: {}",
        got.body
    );

    let b_get = call(
        &app,
        "GET",
        &format!("{}/{a_hid}", hosts_path(&ws_b)),
        Some(&jar_b),
        None,
    )
    .await;
    assert!(
        b_get.status == StatusCode::FORBIDDEN || b_get.status == StatusCode::NOT_FOUND,
        "B GET hid after PATCH CLUSTER/padded must be 403/404: {} {}",
        b_get.status,
        b_get.body
    );

    let ws_b_id = Uuid::parse_str(&ws_b).expect("ws b");
    let _ = enqueue_spawn_for_host(&state, ws_b_id, a_hid).await;
    let a_lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &a_machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(
        &a_lease,
        "A /me/lease after B-only enqueue (PATCH CLUSTER/padded)",
    );
}

/// Leftover labels=[cluster] on a paired laptop, then HTTP PATCH {labels:[gpu]}
/// must persist gpu-only so B GET hid is 404 (HTTP replace, not merge).
#[tokio::test]
async fn http_patch_gpu_on_leftover_cluster_label_unscopes() {
    let (app, state) = app_with_state().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let a_name = format!("a-laptop-{}", Uuid::new_v4());
    let (a_hid, _a_machine) =
        create_pair_and_heartbeat(&app, &jar_a, &ws_a, &a_name, true, false).await;
    let a_path = format!("{}/{a_hid}", hosts_path(&ws_a));

    let planted = fleet_db::repos::worker_hosts::patch_cp_settings(
        &state.db,
        a_hid,
        &fleet_db::HostCpSettings {
            labels: Some(vec!["cluster".to_string()]),
            ..fleet_db::HostCpSettings::default()
        },
    )
    .await
    .expect("plant leftover cluster");
    assert!(
        planted.labels.iter().any(|l| fleet_db::label_is_cluster(l)),
        "setup leftover cluster: {:?}",
        planted.labels
    );

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &a_path,
        Some(&jar_a),
        Some(json!({ "labels": ["gpu"] })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH labels=[gpu] on leftover cluster: {}",
        patched.body
    );
    assert_eq!(
        patched.body["labels"],
        json!(["gpu"]),
        "PATCH labels=[gpu] must persist gpu-only: {}",
        patched.body
    );

    let got = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "A GET after PATCH gpu: {}",
        got.body
    );
    assert_eq!(
        got.body["labels"],
        json!(["gpu"]),
        "GET must stay gpu-only: {}",
        got.body
    );

    let b_get = call(
        &app,
        "GET",
        &format!("{}/{a_hid}", hosts_path(&ws_b)),
        Some(&jar_b),
        None,
    )
    .await;
    assert_eq!(
        b_get.status,
        StatusCode::NOT_FOUND,
        "B GET hid must be 404 after PATCH gpu cleared leftover cluster: {} {}",
        b_get.status,
        b_get.body
    );
}

fn labels_contain(body: &Value, label: &str) -> bool {
    body["labels"]
        .as_array()
        .map(|a| a.iter().any(|v| v.as_str() == Some(label)))
        .unwrap_or(false)
}

fn labels_omit_cluster(body: &Value) -> bool {
    body["labels"]
        .as_array()
        .map(|a| {
            a.iter()
                .all(|v| v.as_str().is_none_or(|s| !fleet_db::label_is_cluster(s)))
        })
        .unwrap_or(true)
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

/// catalog:unit-patch-name — PATCH name of 256/10k bytes is 400 leftover
/// GET seed; exactly 255 is 200.
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
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let name_256 = "x".repeat(256);
    let name_10k = "x".repeat(10_000);
    for (label, name) in [("256", name_256.as_str()), ("10k", name_10k.as_str())] {
        let out = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "PATCH name {label} must be 400: {}",
            out.body
        );
        assert!(
            out.body
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|e| !e.is_empty()),
            "PATCH name {label} must include an error field: {}",
            out.body
        );
        assert_host_name(
            &app,
            &jar,
            &host_path,
            &seed,
            None,
            &format!("after {label} reject"),
        )
        .await;
    }

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
    assert_host_name(&app, &jar, &host_path, &at_max, None, "GET after 255").await;
}

/// catalog:unit-patch-name — cookie PATCH {name: seed} is 200; leftover
/// GET name stays seed and settings_gen is unchanged.
#[tokio::test]
async fn rename_host_patch_identical_name_does_not_bump_settings_gen() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let seed = format!("rename-same-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": seed })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let gen0 = created.body["host"]["settings_gen"]
        .as_i64()
        .expect("settings_gen");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "name": seed })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH identical name must be 200: {}",
        patched.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        Some(gen0),
        "after identical-name PATCH",
    )
    .await;
}

/// catalog:unit-patch-name — cookie PATCH {name: null} is 200; leftover
/// GET name and settings_gen stay the seed (null is omitted).
#[tokio::test]
async fn rename_host_patch_null_name_preserves_seed() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let seed = format!("rename-null-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": seed })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let gen0 = created.body["host"]["settings_gen"]
        .as_i64()
        .expect("settings_gen");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "name": null })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH name null must be 200: {}",
        patched.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        Some(gen0),
        "after null-name PATCH",
    )
    .await;
}

/// catalog:unit-patch-name — cookie PATCH of 128 × 'é' (256 bytes) is 400;
/// leftover GET name stays seed (http_patch uses trimmed.len() bytes).
#[tokio::test]
async fn rename_host_patch_utf8_over_255_bytes_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let seed = format!("rename-utf8-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": seed })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let too_long = "é".repeat(128);
    assert_eq!(too_long.len(), 256, "128 × é must be 256 UTF-8 bytes");
    let out = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "name": too_long })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "PATCH 256-byte UTF-8 name must be 400: {}",
        out.body
    );
    assert!(
        out.body
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty()),
        "PATCH 256-byte UTF-8 name must include an error field: {}",
        out.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed,
        None,
        "after UTF-8 over-255 reject",
    )
    .await;
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
async fn get_synthetic_plane_id_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws uuid");
    let plane_id = fleet_db::control_plane_runner_id(ws_id);

    let got = call(
        &app,
        "GET",
        &format!("{}/{plane_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got.status,
        StatusCode::NOT_FOUND,
        "GET synthetic plane id: {}",
        got.body
    );
}

#[tokio::test]
async fn post_kind_control_plane_is_400_and_inserts_nothing() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let before = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "list before: {}",
        before.body
    );
    let before_ids: Vec<String> = before.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": "plane", "kind": "control_plane" })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::BAD_REQUEST,
        "POST kind=control_plane must be 400: {}",
        created.body
    );

    let list = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "list after POST: {}",
        list.body
    );
    let hosts = list.body["hosts"].as_array().expect("hosts");
    let after_ids: Vec<String> = hosts
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        after_ids, before_ids,
        "POST kind=control_plane must not insert a host: {}",
        list.body
    );
    assert!(
        hosts
            .iter()
            .all(|h| h.get("kind").and_then(|k| k.as_str()) != Some("control_plane")),
        "POST kind=control_plane must not create a plane host: {}",
        list.body
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

/// GET/list/pair/bootstrap/heartbeat/lease must omit pair_token,
/// install_hint, hash keys, and the snapshot flp_ plaintext.
fn assert_http_omits_pair_plaintext(out: &HttpOut, tokens: &[&str], what: &str) {
    assert_eq!(out.status, StatusCode::OK, "{what}: {}", out.body);
    for key in ["pair_token", "pair_token_hash", "install_hint"] {
        assert!(
            !json_has_key(&out.body, key),
            "{what} leaked {key}: {}",
            out.body
        );
    }
    for token in tokens {
        assert!(
            !out.raw.contains(*token),
            "{what} raw body leaked pair token plaintext"
        );
        assert!(
            !json_contains_str(&out.body, token),
            "{what} leaked pair token plaintext: {}",
            out.body
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
/// HTTP PATCH isolation=sandboxed is 200; cookie GET then shows sandboxed.
#[tokio::test]
async fn isolation_defaults_native_and_patch_sandboxed() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let patched = call(
        &app,
        "PATCH",
        &host_path,
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

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "get after patch: {}", got.body);
    assert_eq!(
        got.body["isolation"], "sandboxed",
        "GET after PATCH must show sandboxed: {}",
        got.body
    );
}

/// Enqueued spawn_session stays ready: sandboxed /me/lease is {claim:null}.
#[tokio::test]
async fn sandboxed_paired_host_queued_spawn_lease_is_claim_null_and_ready() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "sandboxed-lease-box", true, false).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "isolation": "sandboxed" })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "patch sandboxed: {}",
        patched.body
    );
    assert_eq!(
        patched.body["isolation"], "sandboxed",
        "PATCH must persist sandboxed before lease: {}",
        patched.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "sandboxed host with queued spawn");
    assert_claim_unleased(
        &state,
        claim_id,
        "sandboxed host must not consume the queued spawn",
    )
    .await;
}

/// PATCH isolation="" is 400; GET and list_assignable stay native.
#[tokio::test]
async fn patch_isolation_empty_string_is_400_native_stays_assignable() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "empty-iso-box", true, false).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "isolation": "" })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::BAD_REQUEST,
        "PATCH isolation=\"\" must be 400: {}",
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
        "GET must still show native after rejected empty isolation: {}",
        got.body
    );

    let listed = worker_hosts_assignable(&state, &ws).await;
    assert_eq!(
        listed,
        vec![host_id],
        "after PATCH isolation=\"\" the paired native is still the only assignable id: {listed:?}"
    );
}

/// POST isolation=sandboxed (PATCH-valid) is 400 and inserts no worker_hosts
/// row. Create rejects the field; PATCH sandboxed stays allowed.
#[tokio::test]
async fn post_isolation_sandboxed_is_400_and_does_not_insert() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let before = workspace_host_ids(&state, ws_id).await;

    let out = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({
            "name": format!("iso-sandboxed-{}", Uuid::new_v4()),
            "isolation": "sandboxed"
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "POST isolation=sandboxed must be 400: {}",
        out.body
    );
    let after = workspace_host_ids(&state, ws_id).await;
    assert_eq!(
        after, before,
        "POST isolation=sandboxed must not insert a worker_hosts row: before={before:?} after={after:?}"
    );
}

/// POST isolation=native is 400 and list_for_workspace stays empty.
/// Create rejects the field entirely, including the default native value.
#[tokio::test]
async fn post_isolation_native_is_400_and_does_not_insert() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let before = workspace_host_ids(&state, ws_id).await;
    assert!(
        before.is_empty(),
        "fresh workspace has no worker_hosts rows: {before:?}"
    );

    let out = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({
            "name": format!("iso-native-{}", Uuid::new_v4()),
            "isolation": "native"
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "POST isolation=native must be 400: {}",
        out.body
    );
    let after = workspace_host_ids(&state, ws_id).await;
    assert!(
        after.is_empty(),
        "POST isolation=native must not insert a worker_hosts row: {after:?}"
    );
}

/// POST kind=cloud / kind=aws is 400, names kind, and inserts no row.
#[tokio::test]
async fn post_kind_cloud_and_aws_http_does_not_create_assignable_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");

    for kind in ["cloud", "aws"] {
        let before = workspace_host_ids(&state, ws_id).await;
        let out = call(
            &app,
            "POST",
            &hosts_path(&ws),
            Some(&jar),
            Some(json!({
                "name": format!("{kind}-box-{}", Uuid::new_v4()),
                "kind": kind
            })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "POST kind={kind} must be 400: {}",
            out.body
        );
        assert!(
            out.body["error"].as_str().unwrap_or("").contains("kind"),
            "error should name kind: {}",
            out.body
        );
        let after = workspace_host_ids(&state, ws_id).await;
        assert_eq!(
            after, before,
            "POST kind={kind} must not insert a worker_hosts row: before={before:?} after={after:?}"
        );
    }
}

/// POST kind=host|gcp|partners|control_plane is 400 and names the kind field.
/// http_create rejects any nonempty kind, not only cloud/aws.
#[tokio::test]
async fn post_kind_host_gcp_partners_control_plane_is_400_and_names_kind() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let before = workspace_host_ids(&state, ws_id).await;
    assert!(
        before.is_empty(),
        "fresh workspace has no worker_hosts rows: {before:?}"
    );

    for kind in ["host", "gcp", "partners", "control_plane"] {
        let out = call(
            &app,
            "POST",
            &hosts_path(&ws),
            Some(&jar),
            Some(json!({
                "name": format!("kind-{kind}-{}", Uuid::new_v4()),
                "kind": kind
            })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "POST kind={kind} must be 400: {}",
            out.body
        );
        let err = out.body["error"].as_str().unwrap_or("");
        assert!(
            err.contains("kind is not a create field"),
            "POST kind={kind} error must name the kind field: {}",
            out.body
        );
        let after = workspace_host_ids(&state, ws_id).await;
        assert!(
            after.is_empty(),
            "POST kind={kind} must not insert a worker_hosts row: {after:?}"
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
) {
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
}

fn assert_pair_expires_at_pair_ttl(
    pair_expires_at: &Value,
    issued_from: chrono::DateTime<chrono::Utc>,
    issued_to: chrono::DateTime<chrono::Utc>,
) {
    let raw = pair_expires_at
        .as_str()
        .unwrap_or_else(|| panic!("pair_expires_at must be an RFC3339 string: {pair_expires_at}"));
    let expires = chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("pair_expires_at must be RFC3339 ({e}): {raw}"))
        .with_timezone(&chrono::Utc);
    let ttl = fleet_db::repos::worker_hosts::PAIR_TTL;
    let lo = issued_from + ttl;
    let hi = issued_to + ttl;
    assert!(
        expires >= lo && expires <= hi,
        "pair_expires_at must be issued_at + fleet_db PAIR_TTL ({ttl}); pair_expires_at={expires} expected {lo}..={hi}"
    );
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

/// HTTP create/rotate must issue `pair_expires_at` in
/// issued_from+1h..=issued_to+1h (catalog hour).
#[tokio::test]
async fn pair_token_expires_in_one_hour_and_cannot_be_reused() {
    let app = app().await;
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
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    assert_pair_expires_in_one_hour(&created.body["pair_expires_at"], issued_from, issued_to);

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
    assert_pair_expires_in_one_hour(&rotated.body["pair_expires_at"], rotate_from, rotate_to);
}

/// HTTP create/rotate must issue `pair_expires_at` in
/// issued_from+fleet_db::PAIR_TTL..=issued_to+fleet_db::PAIR_TTL.
#[tokio::test]
async fn http_create_and_rotate_pair_expires_at_use_fleet_db_pair_ttl() {
    let app = app().await;
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
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    assert_pair_expires_at_pair_ttl(&created.body["pair_expires_at"], issued_from, issued_to);

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
    assert_pair_expires_at_pair_ttl(&rotated.body["pair_expires_at"], rotate_from, rotate_to);
}

/// Whitespace hostname must 400 before consume_pair_token so the oneshot flp_ is not burned.
#[tokio::test]
async fn pair_empty_hostname_or_platform_is_400_and_does_not_consume() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

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

    let out = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, "   ")),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "whitespace hostname must 400 before consume: {}",
        out.body
    );
    assert_pair_body_omits_machine(&out, "POST /pair whitespace hostname");
    assert_host_still_pending(&app, &jar, &ws, &hid, "after whitespace hostname pair").await;

    let unique_hn = format!("ok-hn-{}", Uuid::new_v4());
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, &unique_hn)),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "same flp_ must still 200 after whitespace hostname 400: {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("control pair must return machine_token");
    assert!(
        machine.starts_with("flm_"),
        "control pair machine_token: {machine}"
    );
}

/// Trim-empty hostname must 400 before create_pending so bootstrap does not mint a nameless host.
#[tokio::test]
async fn bootstrap_empty_hostname_or_platform_is_400_and_inserts_no_host() {
    let (app, state) = app_with_state().await;
    let _secret = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let listed_before = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_before.status,
        StatusCode::OK,
        "GET /worker-hosts before empty bootstrap: {}",
        listed_before.body
    );
    let before_ids: Vec<String> = listed_before.body["hosts"]
        .as_array()
        .expect("hosts array")
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    let repo_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let repo_before_ids: Vec<Uuid> = repo_before.iter().map(|h| h.id).collect();

    let out = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": "   ",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": ws,
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "whitespace hostname bootstrap must 400 before create_pending: {}",
        out.body
    );
    assert_pair_body_omits_machine(&out, "POST /bootstrap whitespace hostname");

    let listed_after = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(
        listed_after.status,
        StatusCode::OK,
        "GET /worker-hosts after empty bootstrap: {}",
        listed_after.body
    );
    let after_ids: Vec<String> = listed_after.body["hosts"]
        .as_array()
        .expect("hosts array")
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        after_ids, before_ids,
        "empty bootstrap must not change GET /worker-hosts ids"
    );
    let repo_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let repo_after_ids: Vec<Uuid> = repo_after.iter().map(|h| h.id).collect();
    assert_eq!(
        repo_after_ids, repo_before_ids,
        "empty bootstrap must not insert a worker_hosts row for this workspace"
    );
}

/// Omitted hostname JSON key is serde 422; the live flp_ must stay unused.
#[tokio::test]
async fn pair_omitted_hostname_or_platform_key_is_422_and_does_not_consume() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("omit-hn-{}", Uuid::new_v4()) })),
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

    let out = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "omitted hostname key must be 422: {}",
        out.body
    );
    assert_pair_body_omits_machine(&out, "POST /pair omitted hostname");
    assert_host_still_pending(&app, &jar, &ws, &hid, "after omitted hostname pair").await;

    let unique_hn = format!("ok-hn-{}", Uuid::new_v4());
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&pair_token, &unique_hn)),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "same flp_ must still 200 after omitted hostname 422: {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("control pair must return machine_token");
    assert!(
        machine.starts_with("flm_"),
        "control pair machine_token: {machine}"
    );
}

/// Owner Cookie + body `token=<fleet_access JWT>` must not mint
/// `machine_token` or consume the pending host on guest POST /pair.
#[tokio::test]
async fn pair_cookie_jwt_and_flm_cannot_complete_pair() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("pair-cookie-jwt-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let out = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        Some(&jar),
        Some(pair_body(&access, "cookie-jwt-must-not-pair")),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "owner Cookie + body token=fleet_access must be 401: {}",
        out.body
    );
    assert_pair_body_omits_machine(&out, "owner Cookie + body token=fleet_access");
    assert_host_still_pending(&app, &jar, &ws, &hid, "after Cookie + fleet_access pair").await;
}

/// A well-formed `flp_` that was never inserted must 401 and leave
/// the planted pending row unconsumed.
#[tokio::test]
async fn pair_unknown_never_issued_flp_is_401_and_does_not_consume() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("pair-unknown-flp-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let unknown = fleet_db::repos::worker_hosts::generate_pair_token();

    let out = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&unknown, "unknown-flp-must-not-pair")),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "never-issued flp_ must be 401: {}",
        out.body
    );
    assert_pair_body_omits_machine(&out, "never-issued flp_");
    assert_host_still_pending(&app, &jar, &ws, &hid, "after never-issued flp_").await;
}

/// `Authorization: Bearer` is ignored on POST /pair; a real body `flp_`
/// still pairs and mints `flm_`.
#[tokio::test]
async fn pair_authorization_bearer_is_ignored_when_body_has_real_flp_() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("pair-bearer-ignored-{}", Uuid::new_v4()) })),
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

    let paired = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(access.as_str()),
        Some(pair_body(&pair_token, "bearer-ignored-pair")),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "Bearer fleet_access + body token=real flp_ must still pair: {}",
        paired.body
    );
    assert_eq!(paired.body["host_id"].as_str(), Some(hid.as_str()));
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token");
    assert!(
        machine.starts_with("flm_"),
        "Bearer + real flp_ must mint flm_: {machine}"
    );
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

/// Old binary: omit machine_id. After HTTP DELETE, POST /pair on the same
/// hostname+platform must return the original host_id (hostname fallback).
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
    assert_ne!(hid2, hid, "second create is a new pending row");
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

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let hosts = listed.body["hosts"].as_array().expect("hosts");
    let leftover = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid2.as_str()))
        .expect("leftover pending must stay listed");
    assert_eq!(leftover["status"], "disabled");
}

/// POST /pair machine_id (http_pair parse_machine_id) after DELETE reclaims
/// hid even when hostname changed.
#[tokio::test]
async fn re_pair_same_machine_id_http_reclaims_row() {
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

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "get after pair: {}", got.body);
    let mid = got.body["machine_id"]
        .as_str()
        .unwrap_or_else(|| panic!("first pair must persist machine_id: {}", got.body))
        .to_string();

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
    let token2 = created2.body["pair_token"].as_str().expect("pair_token");
    let again = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": token2,
            "hostname": "studio-pc-renamed",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "machine_id": mid,
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "re-pair: {}", again.body);
    assert_eq!(
        again.body["host_id"].as_str(),
        Some(hid.as_str()),
        "same machine_id must reclaim the original row: {}",
        again.body
    );
}

/// Apply §7: heartbeat `software.updating=true` stops new leases without
/// PATCH paused/drain. `host_accepts_leases` adds `!software_updating`;
/// `list_assignable` has no updating predicate (treating updating as
/// paused would drop the picker). Distinct from paused/drain/enabled
/// siblings.
#[tokio::test]
async fn updating_host_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "updating-lease-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let hb_updating = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
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
        "GET after updating heartbeat: {}",
        got.body
    );
    assert_eq!(
        got.body["software"]["updating"], true,
        "GET software.updating: {}",
        got.body
    );
    assert_eq!(got.body["paused"], false, "GET paused: {}", got.body);

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

    let assignable = worker_hosts_assignable(&state, &ws).await;
    assert!(
        assignable.contains(&host_id),
        "list_assignable must still include an updating host (do not treat updating as paused): {assignable:?}"
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
}

/// After software.updating=true, a later /me/heartbeat that omits
/// software must COALESCE-keep software_updating so /me/lease stays
/// {claim:null}. Overwriting the column would start leases mid-swap.
#[tokio::test]
async fn heartbeat_omit_software_after_updating_keeps_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "omit-sw-updating-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let hb_updating = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
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
        "omit-software heartbeat (COALESCE): {}",
        omit.body
    );

    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after omit-software heartbeat"));
    assert!(
        stored.software_updating,
        "omit-software heartbeat must COALESCE-keep software_updating: {stored:?}"
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
        "GET after omit-software: {}",
        got.body
    );
    assert_eq!(
        got.body["software"]["updating"], true,
        "GET must still show software.updating after omit: {}",
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
    assert_lease_claim_null(&lease, "omit-software after updating=true /me/lease");
    assert_claim_unleased(
        &state,
        queued,
        "COALESCE-kept software_updating must not consume the queued spawn",
    )
    .await;
}

/// catalog:list-expired-host-lease-reclaimable — after `lease_expires_at`
/// is past, the same `flm_` POST `/me/lease` must return that
/// `spawn_session` with a new `lease_token`. `http_lease` reclaims then
/// claims; dropping `reclaim_expired_leases` leaves the row leased.
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
    assert_ne!(
        new_token, old_token,
        "reclaim must issue a new lease_token: {}",
        reclaimed.body
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

/// GET /worker-hosts publishes [verify] package_versions the runners UI
/// copies onto PackageInventory even when the workspace has zero hosts.
/// `http_list` always emits fleet_host/driver_version(); 0.0.0 is the
/// missing-`[verify]` fallback.
#[tokio::test]
async fn empty_workspace_list_still_publishes_package_versions() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let hosts = listed.body["hosts"].as_array().expect("hosts");
    let home: Vec<_> = hosts
        .iter()
        .filter(|h| h["workspace_id"].as_str() == Some(ws.as_str()))
        .collect();
    assert!(
        home.is_empty(),
        "fresh workspace must own zero hosts (this-machine boxes from other workspaces may still list): {}",
        listed.body
    );
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
    assert_ne!(
        listed.body["package_versions"]["fleet-host"], "0.0.0",
        "fleet-host [verify] must not fall back to 0.0.0: {}",
        listed.body
    );
    assert_ne!(
        listed.body["package_versions"]["fleet-driver"], "0.0.0",
        "fleet-driver [verify] must not fall back to 0.0.0: {}",
        listed.body
    );
}

/// ADMIN_TOKEN (`test-admin`) is /v1 admin only. Machine routes require
/// `Authorization: Bearer flm_…` — a valid admin secret must not heartbeat,
/// lease, or complete as every host.
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

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        TEST_ADMIN_TOKEN,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 99,
            "hostname": "admin-must-not-touch"
        })),
    )
    .await;
    assert_admin_rejected_as_machine(&hb, "Bearer test-admin /me/heartbeat");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        TEST_ADMIN_TOKEN,
        Some(json!({})),
    )
    .await;
    assert_admin_rejected_as_machine(&lease, "Bearer test-admin /me/lease");

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
    assert_claim_still_leased(&state, claim_id, &lease_token, "after real flm_ lease").await;

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
    assert_claim_still_leased(&state, claim_id, &lease_token, "after ADMIN_TOKEN complete").await;

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
}

/// Empty `Authorization: Bearer` is missing a token. `/me/heartbeat` and
/// `/me/lease` must 401 `not authenticated` (not `invalid machine token`)
/// and must not persist last_seen / current_jobs.
#[tokio::test]
async fn empty_bearer_me_heartbeat_and_lease_is_401_not_authenticated() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let hid = host_id.to_string();

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

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        "",
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 99,
            "hostname": "empty-bearer-must-not-touch"
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::UNAUTHORIZED,
        "Bearer empty /me/heartbeat must be 401: {}",
        hb.body
    );
    let hb_err = hb.body["error"].as_str().unwrap_or(&hb.raw);
    assert_eq!(
        hb_err, "not authenticated",
        "Bearer empty /me/heartbeat must be 401 not authenticated, got {hb_err:?}: {}",
        hb.body
    );
    assert!(
        !hb_err.contains("invalid machine token"),
        "Bearer empty must not report invalid machine token: {hb_err:?}: {}",
        hb.body
    );
    assert_machine_route_omits_claim_payload(&hb, "Bearer empty /me/heartbeat");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        "",
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::UNAUTHORIZED,
        "Bearer empty /me/lease must be 401: {}",
        lease.body
    );
    let lease_err = lease.body["error"].as_str().unwrap_or(&lease.raw);
    assert_eq!(
        lease_err, "not authenticated",
        "Bearer empty /me/lease must be 401 not authenticated, got {lease_err:?}: {}",
        lease.body
    );
    assert!(
        !lease_err.contains("invalid machine token"),
        "Bearer empty must not report invalid machine token: {lease_err:?}: {}",
        lease.body
    );
    assert_machine_route_omits_claim_payload(&lease, "Bearer empty /me/lease");

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
        "get after empty-bearer probes: {}",
        after.body
    );
    assert_eq!(
        after.body["last_seen_at"].as_str(),
        Some(last_seen.as_str()),
        "empty Bearer heartbeat must not touch last_seen_at: {}",
        after.body
    );
    assert_eq!(
        after.body["current_jobs"].as_i64().unwrap_or(-1),
        i64::from(jobs_before),
        "empty Bearer heartbeat must not persist current_jobs: {}",
        after.body
    );
    let stored_after = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host after empty bearer")
        .unwrap_or_else(|| panic!("host {host_id} missing after empty-bearer probes"));
    assert_eq!(
        stored_after.last_seen_at, seen_at,
        "empty Bearer must not rewrite last_seen_at: {stored_after:?}"
    );
    assert_eq!(
        stored_after.current_jobs, jobs_before,
        "empty Bearer must not rewrite current_jobs: {stored_after:?}"
    );
}

/// Live `fleet_access` JWT and `fleet_refresh` sent as Bearer are not
/// machine tokens. `/me/heartbeat` and `/me/lease` require
/// `Authorization: Bearer flm_…` — a stolen login must not heartbeat or
/// take a spawn. Cookie-only, Cookie+JWT, and flm_ 200s live on siblings.
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
    assert_claim_unleased(&state, claim_id, "enqueued spawn before JWT/refresh probes").await;

    let hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 99,
        "hostname": "cookie-jwt-must-not-touch"
    });
    let lease_body = Some(json!({}));

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
        "get after JWT/refresh probes: {}",
        after.body
    );
    assert_eq!(
        after.body["last_seen_at"].as_str(),
        Some(last_seen.as_str()),
        "JWT/refresh heartbeat must not touch last_seen_at: {}",
        after.body
    );
    assert_eq!(
        after.body["current_jobs"].as_i64().unwrap_or(-1),
        i64::from(jobs_before),
        "JWT/refresh heartbeat must not persist current_jobs: {}",
        after.body
    );
    let stored_after = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host after JWT/refresh")
        .unwrap_or_else(|| panic!("host {host_id} missing after JWT/refresh probes"));
    assert_eq!(
        stored_after.last_seen_at, seen_at,
        "JWT/refresh must not rewrite last_seen_at: {stored_after:?}"
    );
    assert_eq!(
        stored_after.current_jobs, jobs_before,
        "JWT/refresh must not rewrite current_jobs: {stored_after:?}"
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "JWT/refresh /me/lease must not consume the enqueued spawn",
    )
    .await;
}

/// Owner Cookie + Bearer flm_ on POST /me/lease uses the machine token,
/// not the session — 200 and the queued spawn is consumed. Distinct from
/// cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease (JWT/refresh
/// Bearer 401s) and cookie_plus_flm_completes_held_claim (Cookie+flm_ on
/// /complete).
#[tokio::test]
async fn cookie_plus_flm_lease_uses_machine_not_session() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    assert_claim_unleased(&state, claim_id, "enqueued spawn before Cookie+flm_ lease").await;

    let lease = call_inner(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(machine.as_str()),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "Cookie + Bearer flm_ /me/lease: {}",
        lease.body
    );
    let claim = lease.body["claim"].as_object().unwrap_or_else(|| {
        panic!(
            "Cookie + Bearer flm_ must lease the spawn (cookie must not replace flm_): {}",
            lease.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "Cookie + Bearer flm_ claim.id: {}",
        lease.body
    );
    let lease_token = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("Cookie + Bearer flm_ lease_token: {}", lease.body))
        .to_string();
    Uuid::parse_str(&lease_token)
        .unwrap_or_else(|e| panic!("Cookie + Bearer flm_ lease_token UUID {lease_token}: {e}"));
    assert_claim_still_leased(
        &state,
        claim_id,
        &lease_token,
        "Cookie + Bearer flm_ must consume the spawn",
    )
    .await;
}

/// Unused UI pair token (`flp_…`) is not a machine Bearer. `/me/heartbeat`
/// and `/me/lease` must 401 (no host/claim) and must not consume the token —
/// POST /pair of that same flp_ still 200. Cookie-only, used-flp last_seen,
/// and flm_ heartbeat live on siblings.
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

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &pair_token,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 99,
            "hostname": "pair-token-must-not-heartbeat"
        })),
    )
    .await;
    assert_pair_token_rejected_as_machine(&hb, "Bearer <flp_> /me/heartbeat");

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &pair_token,
        Some(json!({})),
    )
    .await;
    assert_pair_token_rejected_as_machine(&lease, "Bearer <flp_> /me/lease");

    assert_host_still_pending(&app, &jar, &ws, &hid, "after flp_ machine probes").await;

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
}

/// After a real `flm_` leases a spawn, Bearer unused or used `flp_`
/// POST `/me/lease/{claim_id}/complete` is 401 and the claim stays leased.
/// Distinct from `pair_token_machine_cannot_authenticate_heartbeat_or_lease`
/// (heartbeat/lease + pair-not-consumed) and admin/cookie complete siblings
/// (they never present flp_).
#[tokio::test]
async fn pair_token_cannot_complete_held_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let unused = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("unused-flp-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        unused.status,
        StatusCode::OK,
        "create unused: {}",
        unused.body
    );
    let unused_flp = unused.body["pair_token"]
        .as_str()
        .expect("unused pair_token")
        .to_string();
    assert!(
        unused_flp.starts_with("flp_"),
        "unused pair_token prefix: {unused_flp}"
    );

    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("used-flp-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create paired: {}",
        created.body
    );
    let used_flp = created.body["pair_token"]
        .as_str()
        .expect("used pair_token")
        .to_string();
    assert!(
        used_flp.starts_with("flp_"),
        "used pair_token prefix: {used_flp}"
    );
    assert_ne!(unused_flp, used_flp, "unused and used flp_ must differ");

    let hostname = format!("flp-complete-hn-{}", Uuid::new_v4());
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": used_flp,
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let host_id = Uuid::parse_str(paired.body["host_id"].as_str().expect("host_id")).expect("uuid");
    let machine = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair must return machine_token: {}", paired.body))
        .to_string();
    assert!(machine.starts_with("flm_"), "pair machine_token: {machine}");

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

    for (token, label) in [
        (unused_flp.as_str(), "Bearer unused <flp_>"),
        (used_flp.as_str(), "Bearer used <flp_>"),
    ] {
        let done = call_bearer(
            &app,
            "POST",
            &complete_path,
            token,
            Some(complete_body.clone()),
        )
        .await;
        assert_pair_token_rejected_as_machine(
            &done,
            &format!("{label} /me/lease/{{claim_id}}/complete"),
        );
        assert_claim_still_leased(
            &state,
            claim_id,
            &lease_token,
            &format!("after {label} complete"),
        )
        .await;
    }
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
    assert_eq!(
        err, "invalid machine token",
        "{what} must be 401 invalid machine token (ADMIN_TOKEN is not flm_), got {err:?}: {}",
        out.body
    );
    assert_machine_route_omits_claim_payload(out, what);
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
/// `sanitize_heartbeat_agents` is the product gate: POST dirty grok, then
/// stored `agent_runtimes` JSONB and the heartbeat raw envelope omit needles.
const HEARTBEAT_AGENT_SECRET_NEEDLES: &[&str] = &[
    "unique-secret",
    "sk-test-unique",
    "unique-at",
    "Bearer unique",
    "unique-refresh",
    "unique-password",
    "unique-pw",
];

const HEARTBEAT_AGENT_OFFICIAL_KEYS: &[&str] = &[
    "id",
    "name",
    "installed",
    "logged_in",
    "models",
    "model_details",
];

fn assert_raw_omits_agent_secrets(raw: &str, what: &str) {
    for needle in HEARTBEAT_AGENT_SECRET_NEEDLES {
        assert!(
            !raw.contains(needle),
            "{what} must not contain {needle}: {raw}"
        );
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
    for agent in arr {
        let obj = agent.as_object().unwrap_or_else(|| {
            panic!("{what} agent must be an object: {agent}");
        });
        for key in obj.keys() {
            assert!(
                HEARTBEAT_AGENT_OFFICIAL_KEYS.contains(&key.as_str()),
                "{what} agent key {key} is not official: {agent}"
            );
        }
    }
}

fn list_host_by_id<'a>(listed: &'a HttpOut, hid: &str, what: &str) -> &'a Value {
    listed.body["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["id"].as_str() == Some(hid))
        .unwrap_or_else(|| panic!("{what} list must include host {hid}: {}", listed.body))
}

/// POST `/me/heartbeat` with token-bearing `agents` must store only official
/// CLI fields. Raw `agent_runtimes` JSONB (not as_public serde drop) and the
/// heartbeat raw envelope omit the secret needles.
#[tokio::test]
async fn heartbeat_agents_omit_tokens_from_persist_and_reads() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "omit-tok-box", true, false).await;
    let stamp = Uuid::new_v4();
    let token = format!("unique-secret-{stamp}");
    let api_key = format!("sk-test-unique-{stamp}");
    let access_token = format!("unique-at-{stamp}");
    let authorization = format!("Bearer unique {stamp}");
    let refresh_token = format!("unique-refresh-{stamp}");
    let password = format!("unique-password-{stamp}");
    let secret = format!("unique-pw-{stamp}");

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
                "refresh_token": refresh_token,
                "password": password,
                "secret": secret,
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
    assert_raw_omits_agent_secrets(&hb.raw, "heartbeat raw envelope");

    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after secret heartbeat"));
    assert_raw_omits_agent_secrets(
        &stored.agent_runtimes.to_string(),
        "persisted agent_runtimes JSONB (storage, not as_public serde drop)",
    );
    assert_grok_inventory(&stored.agent_runtimes, "persisted agent_runtimes JSONB");
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

/// Heartbeat advertised_host=false on an already-leasing box makes
/// /me/lease claim:null even if advertised_container is true. v1 is
/// host mode only. Leftover spawn/chat stay ready; restoring
/// advertised_host claims that leftover.
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
    let chat_queued = enqueue_chat_turn_for_host(&state, workspace_id, host_id).await;

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
        "restored host must claim the still-queued leftover: {}",
        lease.body
    );
}

/// catalog:list-paused-host-lease-null — Owner PATCH `enabled=false` is
/// the disable gate in `host_accepts_leases` (`enabled && !paused && !drain &&
/// online`). An enqueued spawn_session for that hid must yield `{claim: null}`
/// and stay `ready`. Distinct from `paused`/`drain`
/// (list-paused-host-completes-claimed-lease) and from allow_host / updating /
/// at_job_cap. Product failure: a disabled box still starts the job.
#[tokio::test]
async fn paused_host_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "disabled-lease-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let leftover = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH enabled=false: {}",
        patched.body
    );
    assert_eq!(
        patched.body["enabled"], false,
        "PATCH enabled=false must persist: {}",
        patched.body
    );

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH enabled=false /me/lease");
    assert_claim_unleased(
        &state,
        leftover,
        "PATCH enabled=false must not consume the queued spawn_session",
    )
    .await;
}

/// PATCH enabled=false must not 409 a lease already held. `http_complete` is
/// require_machine + complete_owned only — wiring it through
/// `host_accepts_leases` would leave spawn_session leased until the 2-minute
/// reclaim. Distinct from list-paused-host-completes-claimed-lease (paused/drain).
#[tokio::test]
async fn disabled_host_completes_claimed_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "disabled-completes-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

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
        Some(claim_id.to_string().as_str()),
        "claim.id: {}",
        first_lease.body
    );
    let lease_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", first_lease.body))
        .to_string();

    let disabled = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(
        disabled.status,
        StatusCode::OK,
        "owner PATCH enabled=false: {}",
        disabled.body
    );
    assert_eq!(
        disabled.body["enabled"], false,
        "PATCH enabled=false: {}",
        disabled.body
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
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete while enabled=false must be 200 (not host_accepts_leases 409): {}",
        done.body
    );
}

/// catalog:list-paused-host-completes-claimed-lease — Owner PATCH paused
/// must not 409 a lease already held. `http_complete` is require_machine +
/// complete_owned only — wiring it through `host_accepts_leases` would leave
/// spawn_session leased until the 2-minute reclaim. Distinct from
/// list-paused-host-lease-null and from drain/enabled=false complete-while-held.
#[tokio::test]
async fn paused_completes_claimed_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "paused-completes-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

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
        Some(claim_id.to_string().as_str()),
        "claim.id: {}",
        first_lease.body
    );
    let lease_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", first_lease.body))
        .to_string();

    let paused = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
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
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after complete"));
    assert_eq!(
        row.status, "done",
        "complete while paused must mark the claim done: {row:?}"
    );
}

/// PATCH drain must not 409 a lease already held. Same http_complete vs
/// host_accepts_leases contract as paused_completes_claimed_lease.
#[tokio::test]
async fn drain_completes_claimed_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "drain-completes-box", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

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
        Some(claim_id.to_string().as_str()),
        "claim.id: {}",
        first_lease.body
    );
    let lease_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", first_lease.body))
        .to_string();

    let drained = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
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
    assert_eq!(drained.body["drain"], true, "PATCH drain: {}", drained.body);

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
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete while drain must be 200 (not host_accepts_leases 409): {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "complete while drain must be ok:true: {}",
        done.body
    );
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after complete"));
    assert_eq!(
        row.status, "done",
        "complete while drain must mark the claim done: {row:?}"
    );
}

/// Owner PATCH `allow_host=false` is the plane restriction on host jobs
/// (`effective_host` = allow_host && advertised_host). Distinct from
/// paused/drain/enabled=false and from advertised_host=false.
/// list_assignable still includes the hid (holding_new_jobs is both modes
/// off); pick/select skip Process. Restoring allow_host claims that spawn.
#[tokio::test]
async fn allow_host_false_lease_returns_claim_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "allow-host-off", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));
    let queued = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

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
        patched.body["advertised_host"], true,
        "advertised_host must stay true (advertised_host_false_lease_null owns that flip): {}",
        patched.body
    );
    assert_eq!(
        patched.body["allow_container"], true,
        "allow_container stays true so holding_new_jobs is not both-modes-off: {}",
        patched.body
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

    let assignable = worker_hosts_assignable(&state, &ws).await;
    assert!(
        assignable.contains(&host_id),
        "list_assignable must still include allow_host=false when allow_container stays true: {assignable:?}"
    );
    assert_tick_does_not_assign(
        &state,
        workspace_id,
        host_id,
        "after PATCH allow_host=false (advertised_host still true)",
    )
    .await;

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
            "allow_host=true must claim the still-queued spawn (not a tautology that every lease is null): {}",
            lease.body
        )
    });
    assert_eq!(
        claim["id"].as_str(),
        Some(queued.to_string().as_str()),
        "restored host must claim the still-queued spawn: {}",
        lease.body
    );
}

/// PATCH allow_host=false must not 409 a lease already held. `http_complete`
/// is require_machine + complete_owned only — wiring it through
/// `host_accepts_leases` would leave spawn_session leased until reclaim.
/// paused_completes_claimed_lease covers paused/drain/enabled only.
#[tokio::test]
async fn allow_host_false_completes_already_held_lease() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "allow-host-completes", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;

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
        Some(claim_id.to_string().as_str()),
        "claim.id: {}",
        first_lease.body
    );
    let lease_token = first_claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("lease_token: {}", first_lease.body))
        .to_string();

    let restricted = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        Some(json!({ "allow_host": false })),
    )
    .await;
    assert_eq!(
        restricted.status,
        StatusCode::OK,
        "owner PATCH allow_host=false: {}",
        restricted.body
    );
    assert_eq!(
        restricted.body["allow_host"], false,
        "PATCH allow_host=false: {}",
        restricted.body
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
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete while allow_host=false must be 200 (not host_accepts_leases 409): {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "complete while allow_host=false must be ok:true: {}",
        done.body
    );
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after complete"));
    assert_eq!(
        row.status, "done",
        "complete while allow_host=false must mark the claim done: {row:?}"
    );
}

/// Enqueued chat_turn must stay ready after PATCH allow_host=false.
/// The spawn_session body above does not plant chat_turn; advertised_host=false
/// chat leftover is owned by advertised_host_false_lease_null.
#[tokio::test]
async fn allow_host_false_must_not_claim_queued_chat_turn() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "allow-host-chat-null", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let chat_queued = enqueue_chat_turn_for_host(&state, workspace_id, host_id).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/{hid}", hosts_path(&ws)),
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

    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "PATCH allow_host=false /me/lease chat_turn");
    assert_claim_unleased(
        &state,
        chat_queued,
        "allow_host=false must not consume the queued chat_turn",
    )
    .await;
}

/// Plane 1 + advertised 8 + current_jobs=1 is at effective max, so
/// POST /me/lease is 200 {claim:null} and the queued spawn_session stays
/// ready. Under-cap (jobs=0) claims that leftover. Distinct from
/// advertised-0-is-cap-1 (`advertised_zero_jobs_1_lease_null`) and
/// current_jobs > effective (`lease_null_when_current_jobs_exceeds_effective`).
/// Product failure: dropping !at_job_cap from host_accepts_leases lets
/// claim_next_spawn_for_host oversubscribe the box.
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
}

/// Plane 4 + advertised 0 + current_jobs=1 is at cap (0 is 1, not
/// unlimited), so POST /me/lease is 200 {claim:null} and the queued
/// spawn stays ready. Repo siblings never hit /me/lease on this path.
#[tokio::test]
async fn advertised_zero_jobs_1_lease_null() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "adv-zero-cap", true, false).await;
    let hid = host_id.to_string();
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_concurrent": 4 })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH max_concurrent=4: {}",
        patched.body
    );
    assert_eq!(patched.body["max_concurrent"], 4, "{}", patched.body);

    let hb = call_bearer(
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
        hb.status,
        StatusCode::OK,
        "heartbeat advertised 0 + current_jobs=1: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["current_jobs"], 1, "{}", hb.body);

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
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
        "advertised max_concurrent=0 + current_jobs=1 /me/lease",
    );
    assert_claim_unleased(
        &state,
        claim_id,
        "advertised-0 at-cap must not consume the queued spawn",
    )
    .await;
}

/// current_jobs=2 against plane 1 is at cap (`>=`, not `==`), so
/// POST /me/lease is 200 {claim:null} and the queued spawn stays ready.
#[tokio::test]
async fn lease_null_when_current_jobs_exceeds_effective() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) =
        create_pair_and_heartbeat(&app, &jar, &ws, "over-cap", true, false).await;
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

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 2,
            "max_concurrent": 8
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat current_jobs=2: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["current_jobs"], 2, "{}", hb.body);
    assert_eq!(
        hb.body["host"]["max_concurrent"], 1,
        "plane 1 must win over advertised 8: {}",
        hb.body
    );

    let claim_id = enqueue_spawn_for_host(&state, workspace_id, host_id).await;
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_lease_claim_null(&lease, "current_jobs exceeds effective max /me/lease");
    assert_claim_unleased(
        &state,
        claim_id,
        "over-cap host must not consume the queued spawn",
    )
    .await;
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

/// Live `fleet_access` JWT sent as Bearer cannot finish a held lease.
/// `POST /me/lease/{claim_id}/complete` requires `Authorization: Bearer flm_…`
/// — a stolen login must not mark a `spawn_session` done.
/// Distinct from cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease
/// (heartbeat/lease only), lease_complete_garbage_token_keeps_claim
/// (non-UUID lease_token 400 + still leased), and
/// cookie_plus_flm_completes_held_claim (Cookie + flm_ 200).
#[tokio::test]
async fn lease_complete_cookie_jwt_cannot_finish_held_claim() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();

    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
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

    let jwt_only = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &access,
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_user_jwt_rejected_as_machine(
        &jwt_only,
        "Bearer <fleet_access JWT> /me/lease/{claim_id}/complete",
    );
    assert_claim_still_leased(
        &state,
        claim_id,
        &lease_token,
        "after Bearer fleet_access complete",
    )
    .await;
}

/// Owner Cookie + Bearer flm_ on POST /me/lease/{id}/complete uses the
/// machine token, not the session — 200 ok:true and the claim is done.
/// Distinct from cookie_plus_flm_lease_uses_machine_not_session (lease only)
/// and lease_complete_cookie_jwt_cannot_finish_held_claim (JWT Bearer 401).
#[tokio::test]
async fn cookie_plus_flm_completes_held_claim() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
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

    let done = call_inner(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        Some(&jar),
        Some(machine.as_str()),
        Some(json!({
            "lease_token": lease_token,
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "Cookie + Bearer flm_ complete: {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "Cookie + Bearer flm_ complete must be ok:true (cookie must not replace flm_): {}",
        done.body
    );
    let row = fleet_db::repos::work_claims::get(&state.db, claim_id)
        .await
        .expect("get claim")
        .unwrap_or_else(|| panic!("claim {claim_id} missing after complete"));
    assert_eq!(
        row.status, "done",
        "Cookie + Bearer flm_ complete must mark the claim done: {row:?}"
    );
}

/// Non-UUID `lease_token` must 400 before `complete_owned`. A parse skip or
/// complete-then-400 would finish the spawn without the secret; the real
/// `flm_` later 409s.
#[tokio::test]
async fn lease_complete_garbage_token_keeps_claim() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box-a", true, false).await;
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

    let complete_path = format!("/api/worker-hosts/me/lease/{claim_id}/complete");
    let garbage = call_bearer(
        &app,
        "POST",
        &complete_path,
        &machine,
        Some(json!({
            "lease_token": "not-a-uuid",
            "ok": true
        })),
    )
    .await;
    assert_ne!(
        garbage.status,
        StatusCode::CONFLICT,
        "garbage lease_token must not 409 already finished: {}",
        garbage.body
    );
    assert_eq!(
        garbage.status,
        StatusCode::BAD_REQUEST,
        "garbage lease_token must be 400, got {}: {}",
        garbage.status,
        garbage.body
    );
    assert_ne!(
        garbage.body.get("ok"),
        Some(&json!(true)),
        "garbage lease_token must not include ok:true: {}",
        garbage.body
    );
    assert_claim_still_leased(&state, claim_id, &lease_token, "after garbage lease_token").await;

    let done = call_bearer(
        &app,
        "POST",
        &complete_path,
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
        "owner complete after garbage token: {}",
        done.body
    );
    assert_eq!(
        done.body.get("ok"),
        Some(&json!(true)),
        "owner complete must be ok:true: {}",
        done.body
    );
}

#[tokio::test]
async fn install_linux_x86_64_script_ok() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/linux-x86_64.sh", None, None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert!(
        out.raw.starts_with("#!"),
        "GET /install/worker/linux-x86_64.sh must start with a shebang: {}",
        &out.raw[..out.raw.len().min(120)]
    );
    assert!(
        out.raw.contains("FLEET_PLATFORM=linux-x86_64"),
        "GET /install/worker/linux-x86_64.sh must export FLEET_PLATFORM=linux-x86_64: {}",
        &out.raw[..out.raw.len().min(200)]
    );
}

#[tokio::test]
async fn install_named_script_unknown_404() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/no-such-os.sh", None, None).await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("install script not found"),
        "unknown named script must 404 install script not found: {err}"
    );
}

#[tokio::test]
async fn install_unix_common_script_http_ok() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/_unix_common.sh", None, None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
    assert!(
        out.raw.contains("pair --url"),
        "GET /install/worker/_unix_common.sh must contain pair --url: {}",
        &out.raw[..out.raw.len().min(200)]
    );
}

#[tokio::test]
async fn install_named_script_windows_ps1_ok() {
    let app = app().await;
    let out = call(
        &app,
        "GET",
        "/install/worker/windows-x86_64.ps1",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.raw);
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

/// `{dist}/../fleet-host` is a real file; lexical `starts_with` still holds.
/// Dropping `reject_unsafe_name` from `lookup_worker_bin` would 200 the marker.
fn plant_parent_fleet_host_outside_dist() -> (tempfile::TempDir, std::path::PathBuf, &'static str) {
    let root = tempfile::tempdir().expect("tempdir");
    let dist = root.path().join("dist");
    std::fs::create_dir_all(dist.join("linux-x86_64")).unwrap();
    const MARKER: &str = "PLANTED-PARENT-FLEET-HOST-MUST-NOT-LEAK";
    std::fs::write(root.path().join("fleet-host"), MARKER).unwrap();
    (root, dist, MARKER)
}

fn assert_install_bin_dotdot_is_400(out: &HttpOut, uri: &str, marker: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{uri} must not 200 planted parent bytes: {}",
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{uri} must 400 invalid name (not 404): {}",
        out.raw
    );
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("invalid name"),
        "{uri} 400 error must be invalid name: {err}"
    );
    assert!(
        !out.raw.contains(marker),
        "{uri} must not leak planted parent bytes: {}",
        out.raw
    );
}

#[tokio::test]
async fn install_worker_bin_dotdot_rejected() {
    let (_root, dist, marker) = plant_parent_fleet_host_outside_dist();
    let (app, _) = app_with_state_dist(Some(dist)).await;
    for uri in [
        "/install/worker/bin/../fleet-host",
        "/install/worker/bin/linux-x86_64/..",
    ] {
        let out = call(&app, "GET", uri, None, None).await;
        assert_install_bin_dotdot_is_400(&out, uri, marker);
    }
}

#[tokio::test]
async fn install_worker_bin_percent_encoded_dotdot_rejected() {
    let (_root, dist, marker) = plant_parent_fleet_host_outside_dist();
    let (app, _) = app_with_state_dist(Some(dist)).await;
    for uri in [
        "/install/worker/bin/%2e%2e/fleet-host",
        "/install/worker/bin/linux-x86_64/%2e%2e%2ffleet-host",
    ] {
        let out = call(&app, "GET", uri, None, None).await;
        assert_install_bin_dotdot_is_400(&out, uri, marker);
    }
}

/// Public GET `/install/worker/bin/{platform}/{bin}` is host/driver artifacts
/// only. Planted `fleet-server` / `fleet-ui` / `cargo` / `.env` / `passwd`
/// must 404 with no artifact bytes, including native `target/` ELF/PE.
#[tokio::test]
async fn install_bin_allowlist_rejects_control_plane_and_unknown_names() {
    let native = fleet_server::install_routes::native_platform_id();
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let host_payload = b"fake-fleet-host-allowlist-control";
    let forbidden_marker = "PLANTED-CONTROL-PLANE-NOT-A-WORKER";
    let env_marker = "SECRET=planted-control-plane-env";
    let passwd_marker = "root:x:0:0:planted-passwd";
    std::fs::write(plat.join("fleet-host"), host_payload).unwrap();
    for name in ["fleet-server", "fleet-ui", "cargo", "fleet-server.exe"] {
        std::fs::write(
            plat.join(name),
            format!("{forbidden_marker}-{name}").as_bytes(),
        )
        .unwrap();
    }
    std::fs::write(plat.join(".env"), env_marker.as_bytes()).unwrap();
    std::fs::write(plat.join("passwd"), passwd_marker.as_bytes()).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;

    let deny = [
        ("fleet-server", forbidden_marker),
        ("fleet-ui", forbidden_marker),
        ("cargo", forbidden_marker),
        ("fleet-server.exe", forbidden_marker),
        (".env", env_marker),
        ("passwd", passwd_marker),
    ];
    for (name, planted) in deny {
        let uri = format!("/install/worker/bin/linux-x86_64/{name}");
        let out = call(&app, "GET", &uri, None, None).await;
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "{uri} must 404 planted control-plane/secret: {}",
            out.raw
        );
        assert!(
            !out.raw.contains(planted),
            "{uri} must not leak planted bytes: {}",
            out.raw
        );
    }

    let host = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/fleet-host",
        None,
        None,
    )
    .await;
    assert_eq!(
        host.status,
        StatusCode::OK,
        "staged fleet-host is an allowed worker artifact: {}",
        host.raw
    );
    assert_eq!(host.raw.as_bytes(), host_payload);
    let busted = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/fleet-host?h=c53195bf6e692ccf9064566c96301cdb63cb8d84b6b6ef22d348819dc6c1212b",
        None,
        None,
    )
    .await;
    assert_eq!(
        busted.status,
        StatusCode::OK,
        "query-string cache bust must still serve the bin: {}",
        busted.raw
    );
    assert_eq!(busted.raw.as_bytes(), host_payload);

    // Dist unset so lookup can reach workspace target/{release,debug}/{bin}.
    let (app_fallback, _) = app_with_state_dist(None).await;
    for name in ["fleet-server", "fleet-ui", "cargo", "fleet-server.exe"] {
        let uri = format!("/install/worker/bin/{native}/{name}");
        let out = call(&app_fallback, "GET", &uri, None, None).await;
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "unset-dist {uri} must 404: {}",
            out.raw
        );
        let bytes = out.raw.as_bytes();
        assert!(
            !bytes.starts_with(b"\x7fELF") && !bytes.starts_with(b"MZ"),
            "unset-dist {uri} must not serve target/ ELF/PE: {}",
            out.raw
        );
    }
}

/// HEAD shares lookup_worker_bin; a planted fleet-server must not advertise
/// its size as Content-Length.
#[tokio::test]
async fn install_bin_head_rejects_planted_control_plane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let planted = b"PLANTED-FLEET-SERVER-HEAD-MUST-NOT-LEAK";
    std::fs::write(plat.join("fleet-server"), planted).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "HEAD",
        "/install/worker/bin/linux-x86_64/fleet-server",
        None,
        None,
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "HEAD planted fleet-server must 404: {}",
        out.raw
    );
    let cl = out
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_ne!(
        cl,
        planted.len().to_string(),
        "HEAD must not advertise planted fleet-server size: {cl}"
    );
}

/// Allowlisted sidecars staged under dist must 200 with those bytes
/// (not OK||404). Manifest 200 is install_worker_manifest_200_when_staged.
#[tokio::test]
async fn install_bin_allowlist_serves_staged_sidecars() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let sha = b"abc123  fleet-host\n";
    let unwind = b"PLANTED-LIBUNWIND-ALLOWED";
    std::fs::write(plat.join("fleet-host.sha256"), sha).unwrap();
    std::fs::write(plat.join("libunwind.dll"), unwind).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;

    let sha_out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/fleet-host.sha256",
        None,
        None,
    )
    .await;
    assert_eq!(
        sha_out.status,
        StatusCode::OK,
        "staged fleet-host.sha256 must 200: {}",
        sha_out.raw
    );
    assert_eq!(sha_out.raw.as_bytes(), sha);

    let dll_out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/libunwind.dll",
        None,
        None,
    )
    .await;
    assert_eq!(
        dll_out.status,
        StatusCode::OK,
        "staged libunwind.dll must 200: {}",
        dll_out.raw
    );
    assert_eq!(dll_out.raw.as_bytes(), unwind);
}

/// Allowlist includes fleet-driver; a staged file must 200 with those bytes.
#[tokio::test]
async fn install_bin_allowlist_serves_staged_fleet_driver() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    let payload = b"fake-fleet-driver-from-dist";
    std::fs::write(plat.join("fleet-driver"), payload).unwrap();

    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let out = call(
        &app,
        "GET",
        "/install/worker/bin/linux-x86_64/fleet-driver",
        None,
        None,
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::OK,
        "staged fleet-driver must 200: {}",
        out.raw
    );
    assert_eq!(out.raw.as_bytes(), payload);
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
async fn heartbeat_desired_from_sibling_bins_without_manifest_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    std::fs::write(plat.join("fleet-host"), b"sibling-host-bytes").unwrap();
    std::fs::write(plat.join("fleet-driver"), b"sibling-driver-bytes").unwrap();
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
                    "fleet-host": "old-host",
                    "fleet-driver": "old-driver"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    assert!(
        hb.body["desired"]["bins"]["fleet-host"]["sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64),
        "desired must hash sibling fleet-host: {}",
        hb.body
    );
    assert!(
        hb.body["desired"]["bins"]["fleet-driver"]["sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64),
        "desired must hash sibling fleet-driver: {}",
        hb.body
    );
}

#[tokio::test]
async fn pair_without_create_name_uses_hostname() {
    let (app, _) = app_with_state_dist(None).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "send_policy": "owner_only" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let pair_token = created.body["pair_token"].as_str().unwrap();
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": "x1",
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "{}", paired.body);
    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    let name = listed.body["hosts"]
        .as_array()
        .and_then(|a| a.iter().find(|h| h["hostname"] == "x1"))
        .and_then(|h| h["name"].as_str());
    assert_eq!(name, Some("x1"), "listed: {}", listed.body);
}

#[tokio::test]
async fn list_software_status_unknown_with_staged_desired() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let (_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;
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
}

#[tokio::test]
async fn heartbeat_software_status_current_when_bins_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let (id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;
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
                "git_sha": "other",
                "bins": {
                    "fleet-host": "aaa111",
                    "fleet-driver": "driver-sha"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "{}", hb.body);
    assert_eq!(hb.body["host"]["software_status"], "current", "{}", hb.body);
    assert_eq!(hb.body["host"]["outdated"], false, "{}", hb.body);

    let got = call(
        &app,
        "GET",
        &format!("{}/{}", hosts_path(&ws), id),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["software_status"], "current", "{}", got.body);
    assert_eq!(got.body["outdated"], false, "{}", got.body);
    assert_eq!(
        got.body["desired"]["platform"], "linux-x86_64",
        "{}",
        got.body
    );
}

#[tokio::test]
async fn heartbeat_desired_null_when_unstaged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;

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
    assert_eq!(hb.status, StatusCode::OK, "{}", hb.body);
    assert!(
        hb.body.get("desired").is_none() || hb.body["desired"].is_null(),
        "unstaged desired must be null: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["software_status"], "unknown", "{}", hb.body);
}

#[tokio::test]
async fn get_host_behind_after_mismatch_bins() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;

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
                "bins": {
                    "fleet-host": "not-the-staged-sha",
                    "fleet-driver": "not-the-driver-sha"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "{}", hb.body);

    let got = call(
        &app,
        "GET",
        &format!("{}/{}", hosts_path(&ws), id),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["software_status"], "behind", "{}", got.body);
    assert_eq!(got.body["outdated"], true, "{}", got.body);
}

#[tokio::test]
async fn list_software_status_current_after_matching_bins() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_platform_manifest(dir.path(), "linux-x86_64", "aaa111", "stage-tree");
    let (app, _) = app_with_state_dist(Some(dir.path().to_path_buf())).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "old-box", true, false).await;

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
                "git_sha": "other",
                "bins": {
                    "fleet-host": "aaa111",
                    "fleet-driver": "driver-sha"
                }
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "{}", hb.body);

    let listed = call(&app, "GET", &hosts_path(&ws), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let host = listed.body["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "old-box")
        .expect("old-box");
    assert_eq!(host["software_status"], "current", "{}", host);
    assert_eq!(host["outdated"], false, "{}", host);
}

/// Same-ws role=member GET list is 200 with hid; one mutate is 403 + leftover.
/// Create leftover is member_create_403_inserts_no_row; GET detail is
/// member_get_host_detail_is_200_and_omits_secrets.
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
    let paused0 = created.body["host"]["paused"]
        .as_bool()
        .expect("host.paused");

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
    assert!(
        patch
            .body
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty()),
        "member patch must include an error field: {}",
        patch.body
    );

    let leftover = call(&app, "GET", &hosts_path(&ws), Some(&owner_jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "leftover GET after member patch: {}",
        leftover.body
    );
    let leftover_host = leftover.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("leftover list must still have hid: {}", leftover.body));
    assert_eq!(
        leftover_host["paused"], paused0,
        "paused unchanged: {}",
        leftover.body
    );
}

/// Same-ws role=member GET /worker-hosts/{hid} is 200 with id and no
/// pair_token / machine_token_hash.
#[tokio::test]
async fn member_get_host_detail_is_200_and_omits_secrets() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&owner_jar),
        Some(json!({ "name": format!("member-detail-{}", Uuid::new_v4()) })),
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

    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "member get: {}", got.body);
    assert_eq!(got.body["id"].as_str(), Some(hid.as_str()), "{}", got.body);
    assert!(
        !json_has_key(&got.body, "pair_token") && !json_has_key(&got.body, "machine_token_hash"),
        "member get must not include pair_token / machine_token_hash: {}",
        got.body
    );
}

/// Same-ws role=member POST /worker-hosts is 403 and must not insert a row.
#[tokio::test]
async fn member_create_403_inserts_no_row() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let nope = format!("nope-{}", Uuid::new_v4());

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

    let create = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&member_jar),
        Some(json!({ "name": nope })),
    )
    .await;
    assert_eq!(
        create.status,
        StatusCode::FORBIDDEN,
        "member create: {}",
        create.body
    );
    assert!(
        create
            .body
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty()),
        "member create must include an error field: {}",
        create.body
    );

    let owner_list = call(&app, "GET", &hosts_path(&ws), Some(&owner_jar), None).await;
    assert_eq!(
        owner_list.status,
        StatusCode::OK,
        "owner list after member create: {}",
        owner_list.body
    );
    assert!(
        owner_list.body["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .all(|h| h["name"].as_str() != Some(nope.as_str())),
        "member create name must not appear on owner list: {}",
        owner_list.body
    );
}

/// Same-ws role=admin POST pair-tokens is 403; leftover owner list stays
/// pending/unpaused and owner mint is still 200 flp_.
#[tokio::test]
async fn admin_cannot_mutate_worker_hosts() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let owner_name = format!("owner-box-{}", Uuid::new_v4());

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

    let rotate = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&admin_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotate.status,
        StatusCode::FORBIDDEN,
        "admin pair-tokens: {}",
        rotate.body
    );
    assert!(
        rotate
            .body
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty()),
        "admin pair-tokens must include an error field: {}",
        rotate.body
    );
    assert!(
        rotate.body.get("pair_token").is_none(),
        "admin pair-tokens 403 must not mint pair_token: {}",
        rotate.body
    );

    let leftover = call(&app, "GET", &hosts_path(&ws), Some(&owner_jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "leftover GET after admin pair-tokens: {}",
        leftover.body
    );
    let leftover_host = leftover.body["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("leftover list must still have hid: {}", leftover.body));
    assert_eq!(
        leftover_host["status"], status0,
        "still pending: {}",
        leftover.body
    );
    assert_eq!(
        leftover_host["paused"], paused0,
        "paused unchanged: {}",
        leftover.body
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
        "owner pair-tokens after admin 403: {}",
        minted.body
    );
    let flp = minted.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("owner pair-tokens must mint flp_: {}", minted.body));
    assert!(flp.starts_with("flp_"), "owner mint prefix: {flp}");
}

/// POST pair-tokens marks the unused create-time flp_ used; POST /pair of
/// that token is 409 already used (a DELETE of the old row is 401).
#[tokio::test]
async fn new_pair_token_old_token_dies() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-old-{}", Uuid::new_v4()) })),
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
        Some(pair_body(&old, "box")),
    )
    .await;
    assert_eq!(
        stale.status,
        StatusCode::CONFLICT,
        "old token after rotate must be 409 already used, not {}: {}",
        stale.status,
        stale.body
    );
    let err = stale.body["error"].as_str().unwrap_or(&stale.raw);
    assert!(
        err.contains("already used"),
        "old token after rotate must say already used (DELETE of the row is 401): {err}"
    );
}

/// POST pair-tokens after pair is 409 (replace_pair_token pending gate).
#[tokio::test]
async fn new_pair_token_after_pair_is_409() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-online-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();
    let token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
        .to_string();

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&token, "box")),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);

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

/// POST pair-tokens twice while pending; first new POST /pair is 409
/// already used; second new POST /pair is 200 (an UPDATE that only
/// retired the create-time row stays green if we only rotate once).
#[tokio::test]
async fn new_pair_token_second_rotate_while_pending_kills_previous_new() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-twice-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let first = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "first rotate: {}", first.body);
    let first_new = first.body["pair_token"]
        .as_str()
        .expect("first new token")
        .to_string();

    let second = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "second rotate: {}",
        second.body
    );
    let second_new = second.body["pair_token"]
        .as_str()
        .expect("second new token")
        .to_string();
    assert_ne!(
        second_new, first_new,
        "second rotate must mint another token"
    );

    let stale = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&first_new, "box")),
    )
    .await;
    assert_eq!(
        stale.status,
        StatusCode::CONFLICT,
        "first new after second rotate must be 409 already used, not {}: {}",
        stale.status,
        stale.body
    );
    let err = stale.body["error"].as_str().unwrap_or(&stale.raw);
    assert!(
        err.contains("already used"),
        "first new after second rotate must say already used: {err}"
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&second_new, "box")),
    )
    .await;
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "second new must still pair: {}",
        paired.body
    );
}

/// POST pair-tokens after DELETE revoke is 409 (require_mutable_host still
/// returns the disabled row; replace_pair_token pending gate fires).
#[tokio::test]
async fn new_pair_token_after_revoke_is_409() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-revoke-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "create: {}", created.body);
    let hid = created.body["host"]["id"]
        .as_str()
        .expect("host.id")
        .to_string();

    let revoked = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "revoke: {}", revoked.body);

    let rotated = call(
        &app,
        "POST",
        &format!("{}/{hid}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotated.status,
        StatusCode::CONFLICT,
        "rotate after revoke: {}",
        rotated.body
    );
}

/// pending A and B; rotate A; B's unused token still POST /pair 200
/// (replace_pair_token UPDATE is WHERE host_id=$1).
#[tokio::test]
async fn new_pair_token_does_not_invalidate_other_hosts_unused_token() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created_a = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-a-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created_a.status,
        StatusCode::OK,
        "create A: {}",
        created_a.body
    );
    let hid_a = created_a.body["host"]["id"]
        .as_str()
        .expect("A host.id")
        .to_string();

    let created_b = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("rotate-b-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created_b.status,
        StatusCode::OK,
        "create B: {}",
        created_b.body
    );
    let token_b = created_b.body["pair_token"]
        .as_str()
        .expect("B pair_token")
        .to_string();

    let rotated_a = call(
        &app,
        "POST",
        &format!("{}/{hid_a}/pair-tokens", hosts_path(&ws)),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rotated_a.status,
        StatusCode::OK,
        "rotate A: {}",
        rotated_a.body
    );

    let paired_b = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(pair_body(&token_b, "box-b")),
    )
    .await;
    assert_eq!(
        paired_b.status,
        StatusCode::OK,
        "B unused token must still pair after A rotate: {}",
        paired_b.body
    );
}

/// catalog:unit-request-update — Auto-update off + Update now (`request_update`).
#[tokio::test]
async fn request_update_marks_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plat = dir.path().join("linux-x86_64");
    std::fs::create_dir_all(&plat).unwrap();
    std::fs::write(
        plat.join("manifest.json"),
        r#"{"git_sha":"staged","platform":"linux-x86_64","bins":{"fleet-host":{"sha256":"abc","version":"9.9.9"},"fleet-driver":{"sha256":"def","version":"9.9.9"}}}"#,
    )
    .unwrap();
    let app = app_with_state_dist(Some(dir.path().to_path_buf())).await.0;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let off = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "auto_update": false })),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK, "auto off: {}", off.body);
    assert_eq!(off.body["auto_update"], false, "{}", off.body);
    let gen_after_auto = off.body["settings_gen"].as_i64().unwrap_or(0);

    let patched = call(
        &app,
        "PATCH",
        &host_path,
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

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "get after: {}", got.body);
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
}

/// Same-ws role=member PATCH request_update=true is 403 and must not stamp.
#[tokio::test]
async fn member_patch_request_update_is_403() {
    let (app, state) = app_with_state().await;
    let (_email, owner_jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) =
        create_pair_and_heartbeat(&app, &owner_jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

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

    let patch = call(
        &app,
        "PATCH",
        &host_path,
        Some(&member_jar),
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(
        patch.status,
        StatusCode::FORBIDDEN,
        "member PATCH request_update: {}",
        patch.body
    );

    let leftover = call(&app, "GET", &host_path, Some(&owner_jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "owner GET: {}",
        leftover.body
    );
    assert!(
        leftover.body.get("update_requested_at").is_none()
            || leftover.body["update_requested_at"].is_null(),
        "member PATCH must not stamp: {}",
        leftover.body
    );
}

/// Update now with default auto_update still on must stamp without flipping auto or gen.
#[tokio::test]
async fn request_update_true_while_auto_on_stamps() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let before = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(before.status, StatusCode::OK, "get: {}", before.body);
    assert_eq!(
        before.body["auto_update"], false,
        "paired host defaults auto-update off: {}",
        before.body
    );
    let gen_before = before.body["settings_gen"].as_i64().unwrap_or(0);

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "request: {}", patched.body);
    assert_eq!(
        patched.body["auto_update"], false,
        "request_update must not turn auto_update on: {}",
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
        gen_before,
        "Update now is a one-shot, not a settings bump: {}",
        patched.body
    );
}

/// request_update=false after Update now must keep the stamp and settings_gen.
#[tokio::test]
async fn request_update_false_after_true_keeps_stamp() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let stamped = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "request_update": true })),
    )
    .await;
    assert_eq!(stamped.status, StatusCode::OK, "request: {}", stamped.body);
    let requested = stamped.body["update_requested_at"]
        .as_str()
        .unwrap_or_else(|| panic!("update_requested_at: {}", stamped.body))
        .to_string();
    let settings_gen = stamped.body["settings_gen"].as_i64().unwrap_or(0);

    let cleared = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "request_update": false })),
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "false: {}", cleared.body);
    assert_eq!(
        cleared.body["update_requested_at"].as_str(),
        Some(requested.as_str()),
        "request_update=false must keep the stamp: {}",
        cleared.body
    );
    assert_eq!(
        cleared.body["settings_gen"].as_i64().unwrap_or(0),
        settings_gen,
        "request_update=false must not bump gen: {}",
        cleared.body
    );
}

/// Unset FLEET_BOOTSTRAP_SECRET 404s POST /bootstrap and inserts no host.
#[tokio::test]
async fn bootstrap_secret_unset_disables_route() {
    let (app, state) = app_with_state().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let listed_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let listed_before_ids: Vec<Uuid> = listed_before.iter().map(|h| h.id).collect();
    let hostname = format!("unset-boot-{}", Uuid::new_v4());

    let _env = BootstrapSecretEnvGuard::lock_and_set(None).await;
    let out = call(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        None,
        Some(json!({
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "unset FLEET_BOOTSTRAP_SECRET must 404 POST /api/worker-hosts/bootstrap: {}",
        out.body
    );

    let listed_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let listed_after_ids: Vec<Uuid> = listed_after.iter().map(|h| h.id).collect();
    assert_eq!(
        listed_after_ids, listed_before_ids,
        "unset bootstrap must not insert a worker_hosts row"
    );
    assert!(
        listed_after
            .iter()
            .all(|h| h.hostname.as_deref() != Some(hostname.as_str())),
        "unset bootstrap must not persist hostname {hostname}: {:?}",
        listed_after
            .iter()
            .map(|h| (h.id, h.hostname.clone()))
            .collect::<Vec<_>>()
    );
}

/// Owner Cookie or fleet_access JWT is not FLEET_BOOTSTRAP_SECRET.
#[tokio::test]
async fn bootstrap_session_cookie_or_jwt_not_accepted_when_secret_configured() {
    let (app, state) = app_with_state().await;
    let _env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let listed_before = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace before");
    let listed_before_ids: Vec<Uuid> = listed_before.iter().map(|h| h.id).collect();
    let hostname = format!("session-boot-{}", Uuid::new_v4());
    let body = json!({
        "hostname": hostname,
        "platform": "linux-x86_64",
        "advertised_host": true,
        "advertised_container": false,
        "workspace_id": ws
    });

    let cookie_only = call(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        Some(&jar),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        cookie_only.status,
        StatusCode::UNAUTHORIZED,
        "owner Cookie must not pair when FLEET_BOOTSTRAP_SECRET is set: {}",
        cookie_only.body
    );

    let access = jar
        .access()
        .expect("signup login must set fleet_access")
        .to_string();
    let bearer_jwt = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        &access,
        Some(body),
    )
    .await;
    assert_eq!(
        bearer_jwt.status,
        StatusCode::UNAUTHORIZED,
        "fleet_access JWT must not pair when FLEET_BOOTSTRAP_SECRET is set: {}",
        bearer_jwt.body
    );

    let listed_after = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace after");
    let listed_after_ids: Vec<Uuid> = listed_after.iter().map(|h| h.id).collect();
    assert_eq!(
        listed_after_ids, listed_before_ids,
        "session bootstrap must not insert a worker_hosts row"
    );
    assert!(
        listed_after
            .iter()
            .all(|h| h.hostname.as_deref() != Some(hostname.as_str())),
        "session bootstrap must not persist hostname {hostname}: {:?}",
        listed_after
            .iter()
            .map(|h| (h.id, h.hostname.clone()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn bootstrap_pairs_and_rotates_same_machine() {
    let app = app().await;
    let _boot_env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let hostname = format!("cluster-host-{}", Uuid::new_v4());
    let first = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": false,
            "advertised_container": true,
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
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": false,
            "advertised_container": true,
            "machine_id": host_id,
            "workspace_id": ws
        })),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::FORBIDDEN,
        "bootstrap must not rotate an existing machine: {}",
        again.body
    );
    assert!(
        again.body["machine_token"].is_null(),
        "refused bootstrap must not mint a token: {}",
        again.body
    );
    let _ = host_id;

    let still = call_bearer(
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
        still.status,
        StatusCode::OK,
        "original machine token still works: {}",
        still.body
    );
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
/// path must 403 home-workspace on one PATCH, DELETE, and pair-tokens
/// of a bootstrap cluster host. A's GET state is unchanged. B GET-by-id
/// is 200 and omits tokens. List injection is
/// list-cluster-host-not-injected. Non-cluster foreign id is
/// list-host-foreign-id-404-omits-workspace.
#[tokio::test]
async fn cluster_host_foreign_workspace_cannot_pause_or_revoke() {
    let app = app().await;
    let (_email_a, jar_a, ws_a) = signup_and_login(&app).await;
    let hostname = format!("cluster-foreign-{}", Uuid::new_v4());

    let boot_secret = TEST_BOOTSTRAP_SECRET.to_string();
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
    let hid = boot.body["host_id"]
        .as_str()
        .unwrap_or_else(|| panic!("bootstrap host_id: {}", boot.body))
        .to_string();

    let a_path = format!("{}/{hid}", hosts_path(&ws_a));
    let snap = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET after bootstrap: {}",
        snap.body
    );
    assert_eq!(snap.body["id"].as_str(), Some(hid.as_str()));
    assert!(
        labels_contain(&snap.body, "cluster"),
        "A GET labels must contain cluster: {}",
        snap.body
    );
    let original_name = snap.body["name"]
        .as_str()
        .unwrap_or_else(|| panic!("A GET name: {}", snap.body))
        .to_string();
    let original_paused = snap.body["paused"].clone();
    let original_drain = snap.body["drain"].clone();
    let original_enabled = snap.body["enabled"].clone();
    let original_status = snap.body["status"].clone();

    let (_email_b, jar_b, ws_b) = signup_and_login(&app).await;
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");
    let b_path = format!("{}/{hid}", hosts_path(&ws_b));

    let patched = call(
        &app,
        "PATCH",
        &b_path,
        Some(&jar_b),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_cluster_host_mutate_rejected(&patched, "B PATCH paused on B's path");

    let deleted = call(&app, "DELETE", &b_path, Some(&jar_b), None).await;
    assert_cluster_host_mutate_rejected(&deleted, "B DELETE on B's path");

    let rotate = call(
        &app,
        "POST",
        &format!("{b_path}/pair-tokens"),
        Some(&jar_b),
        None,
    )
    .await;
    assert_cluster_host_mutate_rejected(&rotate, "B POST pair-tokens on B's path");

    let after = call(&app, "GET", &a_path, Some(&jar_a), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "A GET after B mutate: {}",
        after.body
    );
    assert_eq!(after.body["id"].as_str(), Some(hid.as_str()));
    assert_eq!(
        after.body["status"], original_status,
        "status unchanged: {}",
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

    let b_get = call(&app, "GET", &b_path, Some(&jar_b), None).await;
    assert_eq!(
        b_get.status,
        StatusCode::OK,
        "B GET cluster guest must be 200: {}",
        b_get.body
    );
    assert_eq!(
        b_get.body["id"].as_str(),
        Some(hid.as_str()),
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
}

/// POST /bootstrap with a guessed workspace_id must 404 and mint no cluster host.
#[tokio::test]
async fn bootstrap_unknown_workspace_id_is_404_and_inserts_no_host() {
    let (app, state) = app_with_state().await;
    let _env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let unknown = Uuid::new_v4();
    assert_ne!(
        unknown, workspace_id,
        "garbage workspace_id must not be A's workspace"
    );
    let hostname = format!("unknown-ws-boot-{}", Uuid::new_v4());
    let out = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        TEST_BOOTSTRAP_SECRET,
        Some(json!({
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": unknown
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "Bearer configured secret + unknown workspace_id must 404, not 200/500: {}",
        out.body
    );
    assert!(
        !json_has_key(&out.body, "machine_token"),
        "unknown workspace_id must not mint machine_token: {}",
        out.body
    );

    let ghost = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, unknown)
        .await
        .expect("list_for_workspace(unknown)");
    assert!(
        ghost.is_empty(),
        "guessed workspace_id must not mint a worker_hosts row: {:?}",
        ghost
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone()))
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
}

/// Wrong bootstrap secret must 401 before workspace lookup so a guessed id does not leak.
#[tokio::test]
async fn bootstrap_wrong_secret_unknown_workspace_is_401_not_404() {
    let (app, state) = app_with_state().await;
    let _env = BootstrapSecretEnvGuard::lock_and_set(Some(TEST_BOOTSTRAP_SECRET)).await;
    let unknown = Uuid::new_v4();
    let hostname = format!("wrong-secret-unknown-{}", Uuid::new_v4());
    let out = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/bootstrap",
        "wrong-secret",
        Some(json!({
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false,
            "workspace_id": unknown
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "wrong secret + guessed workspace_id must 401, not 404: {}",
        out.body
    );
    assert!(
        !json_has_key(&out.body, "machine_token"),
        "wrong secret must not mint machine_token: {}",
        out.body
    );

    let ghost = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, unknown)
        .await
        .expect("list_for_workspace(unknown)");
    assert!(
        ghost.is_empty(),
        "wrong secret + guessed workspace_id must not mint a worker_hosts row: {:?}",
        ghost
            .iter()
            .map(|h| (h.id, h.name.clone(), h.hostname.clone()))
            .collect::<Vec<_>>()
    );
}

fn install_request_spec(body: &Value) -> Option<&str> {
    body.get("install_request")
        .and_then(|v| v.get("spec"))
        .and_then(|v| v.as_str())
}

fn heartbeat_install_spec(body: &Value) -> Option<&str> {
    body.get("install")
        .and_then(|v| v.get("spec"))
        .and_then(|v| v.as_str())
}

/// catalog:unit-install-ack — heartbeat `install_ack` omit/empty/whitespace/
/// wrong spec must 200 and leave GET install_request.spec. Matching spec
/// clears; old spec after re-queue does not.
#[tokio::test]
async fn install_ack_mismatch_keeps_queued_request() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let queued = "llama3.2:latest";

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": queued }
        })),
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
    assert_install_request_is(&app, &jar, &host_path, Some(queued), "GET after queue").await;

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
        assert_eq!(
            hb.status,
            StatusCode::OK,
            "install_ack {label} must 200: {}",
            hb.body
        );
        assert_install_request_is(
            &app,
            &jar,
            &host_path,
            Some(queued),
            &format!("GET after install_ack {label}"),
        )
        .await;
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
    assert_eq!(
        match_hb.status,
        StatusCode::OK,
        "matching install_ack must 200: {}",
        match_hb.body
    );
    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        None,
        "matching install_ack must clear GET install_request",
    )
    .await;

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
    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        Some(second),
        "GET after second queue",
    )
    .await;

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
    assert_eq!(
        old_spec_ack.status,
        StatusCode::OK,
        "ack of the previous spec must 200: {}",
        old_spec_ack.body
    );
    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        Some(second),
        "old-spec ack must leave the re-queued spec",
    )
    .await;
}

/// Runner reads `resp.install.spec` on the same beat; omit/wrong ack must
/// still return the queued string (GET host card is the sibling).
#[tokio::test]
async fn heartbeat_mismatch_ack_response_install_still_queued() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let queued = "llama3.2:latest";
    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH install_model: {}",
        patched.body
    );

    let mismatch_acks = [
        ("omitted spec", json!({ "ok": true })),
        (
            "wrong spec",
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
        assert_eq!(
            hb.status,
            StatusCode::OK,
            "install_ack {label} must 200: {}",
            hb.body
        );
        assert_eq!(
            heartbeat_install_spec(&hb.body),
            Some(queued),
            "install_ack {label} 200 body.install.spec must stay the queued string (runner reads resp.install): {}",
            hb.body
        );
    }
}

/// Host sends `ok: false` on ollama_pull failure. SUT ignores `ack.ok`, so a
/// matching spec still clears Apply (failed pull does not re-serve).
#[tokio::test]
async fn install_ack_ok_false_matching_spec() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let queued = "llama3.2:latest";
    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH install_model: {}",
        patched.body
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
            "install_ack": { "ok": false, "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "matching install_ack ok:false must 200: {}",
        hb.body
    );
    assert!(
        hb.body.get("install").is_none() || hb.body["install"].is_null(),
        "matching install_ack ok:false must not re-serve body.install: {}",
        hb.body
    );
    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        None,
        "matching install_ack ok:false still clears Apply (SUT ignores ack.ok)",
    )
    .await;
}

/// Matching ack 200 `install` must be null so queue_install is not re-fired
/// on the same beat (GET after is the sibling).
#[tokio::test]
async fn heartbeat_matching_ack_response_install_is_null() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));
    let queued = "llama3.2:latest";
    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "owner PATCH install_model: {}",
        patched.body
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
            "install_ack": { "ok": true, "spec": queued }
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "matching install_ack must 200: {}",
        hb.body
    );
    assert!(
        hb.body.get("install").is_none() || hb.body["install"].is_null(),
        "matching install_ack 200 body.install must be null so queue_install is not re-fired: {}",
        hb.body
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

/// Owner PATCH install_model empty / non-ollama / >512 / control / ;|&
/// is 400 and GET spec stays the queued llama3.2:latest. One safe persist
/// so this is not a tautology that every PATCH 400s. `;`, `|`, and `&`
/// are each independently 400.
#[tokio::test]
async fn install_model_rejects_shell_metachar_empty_and_non_ollama() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

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
        "safe PATCH install_model llama3.2:latest must persist: {}",
        persist.body
    );
    assert_eq!(
        install_request_spec(&persist.body),
        Some(safe),
        "safe PATCH must queue llama3.2:latest: {}",
        persist.body
    );
    assert_install_request_is(&app, &jar, &host_path, Some(safe), "GET after safe persist").await;

    let oversize: String = "oversize-install-"
        .chars()
        .chain(std::iter::repeat('x'))
        .take(513)
        .collect();
    assert_eq!(oversize.len(), 513, "oversize spec must be 513 bytes");

    let rejects: &[(&str, Value)] = &[
        (
            "empty spec",
            json!({ "install_model": { "provider": "ollama", "spec": "" } }),
        ),
        (
            "whitespace spec",
            json!({ "install_model": { "provider": "ollama", "spec": "   " } }),
        ),
        (
            "provider grok",
            json!({ "install_model": { "provider": "grok", "spec": "grok-4-install-unique" } }),
        ),
        (
            "spec 513 bytes",
            json!({ "install_model": { "provider": "ollama", "spec": oversize } }),
        ),
        (
            "embedded newline",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest\ncurl-evil-install" } }),
        ),
        (
            "embedded nul",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest\u{0000}nul-install" } }),
        ),
        (
            "semicolon",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest;rm-install" } }),
        ),
        (
            "pipe",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest|curl-install" } }),
        ),
        (
            "ampersand",
            json!({ "install_model": { "provider": "ollama", "spec": "llama3.2:latest&id-install" } }),
        ),
    ];

    for (label, body) in rejects {
        let rejected = call(&app, "PATCH", &host_path, Some(&jar), Some(body.clone())).await;
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "install_model {label} must be 400: {}",
            rejected.body
        );
        assert_install_request_is(
            &app,
            &jar,
            &host_path,
            Some(safe),
            &format!("after owner reject {label}"),
        )
        .await;
    }
}

/// Owner PATCH spec with surrounding spaces queues the trimmed name.
#[tokio::test]
async fn install_model_trims_spec_before_queue() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let persist = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": "  llama3.2:latest  " }
        })),
    )
    .await;
    assert_eq!(
        persist.status,
        StatusCode::OK,
        "padded spec must trim and queue: {}",
        persist.body
    );
    assert_eq!(
        install_request_spec(&persist.body),
        Some("llama3.2:latest"),
        "PATCH must queue the trimmed spec: {}",
        persist.body
    );
    assert_install_request_is(
        &app,
        &jar,
        &host_path,
        Some("llama3.2:latest"),
        "GET after padded spec",
    )
    .await;
}

/// Repo bump list omits install_model; a safe queue must leave settings_gen.
#[tokio::test]
async fn install_model_safe_spec_does_not_bump_settings_gen() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let gen0 = seed.body["settings_gen"].as_i64().expect("settings_gen");

    let persist = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "install_model": { "provider": "ollama", "spec": "llama3.2:latest" }
        })),
    )
    .await;
    assert_eq!(
        persist.status,
        StatusCode::OK,
        "safe PATCH install_model: {}",
        persist.body
    );
    assert_eq!(
        persist.body["settings_gen"].as_i64().expect("settings_gen"),
        gen0,
        "install_model must not bump settings_gen: {}",
        persist.body
    );
    let leftover = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "GET after safe persist: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["settings_gen"]
            .as_i64()
            .expect("settings_gen"),
        gen0,
        "GET settings_gen must stay seed: {}",
        leftover.body
    );
}

/// Validation runs before patch_cp_settings, so a reject must not rename.
#[tokio::test]
async fn install_model_400_does_not_apply_sibling_fields() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let seed_name = seed.body["name"].as_str().expect("name").to_string();
    assert!(
        install_request_spec(&seed.body).is_none(),
        "paired host starts with no install_request: {}",
        seed.body
    );

    let renamed = format!("renamed-{}", Uuid::new_v4());
    let rejected = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "name": renamed,
            "install_model": { "provider": "ollama", "spec": "llama3.2:latest;rm" }
        })),
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "PATCH name + unsafe spec must be 400: {}",
        rejected.body
    );
    assert_host_name(
        &app,
        &jar,
        &host_path,
        &seed_name,
        None,
        "after 400 sibling fields",
    )
    .await;
    assert_install_request_is(&app, &jar, &host_path, None, "after 400 sibling fields").await;
}

/// Owner PATCH runtime_settings with ;|& is 400 and does not persist.
/// One safe work_dir 200 so this is not a tautology that every PATCH 400s.
#[tokio::test]
async fn patch_runtime_settings_rejects_shell_metachar_and_persists_safe_work_dir() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let seed_settings = seed
        .body
        .get("runtime_settings")
        .cloned()
        .unwrap_or(Value::Null);

    let rejected = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({
            "runtime_settings": {
                "work_dir": "/tmp;rm -rf /",
                "docker_image": "img|curl evil",
                "ollama_models_dir": "/var/x&y",
            }
        })),
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "owner PATCH ;|& runtime_settings must be 400: {}",
        rejected.body
    );
    let after_reject = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after_reject.status,
        StatusCode::OK,
        "GET after ;|& reject: {}",
        after_reject.body
    );
    assert_eq!(
        after_reject
            .body
            .get("runtime_settings")
            .unwrap_or(&Value::Null),
        &seed_settings,
        ";|& reject must not persist runtime_settings: {}",
        after_reject.body
    );

    let safe_work_dir = "/var/lib/fleet-host/work";
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
        "safe PATCH work_dir must 200: {}",
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
        "GET must show persisted work_dir: {}",
        after_safe.body
    );
}

/// POST /me/heartbeat runtime with extra_env / machine_token / ;|& work_dir
/// is 200; desired runtime_settings is unchanged; stored runtime_reported
/// JSONB omits those keys/needles (GET/list re-sanitize via host_json_for_viewer).
#[tokio::test]
async fn heartbeat_runtime_reported_stored_jsonb_omits_extra_env_and_tokens() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let seed_settings = seed
        .body
        .get("runtime_settings")
        .cloned()
        .unwrap_or(Value::Null);

    let stamp = Uuid::new_v4();
    let unique_flm = format!("unique-flm-{stamp}");
    let unique_secret = format!("unique-secret-{stamp}");
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
        "heartbeat with dirty runtime must 200: {}",
        hb.body
    );

    let after = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET after dirty heartbeat: {}",
        after.body
    );
    assert_eq!(
        after.body.get("runtime_settings").unwrap_or(&Value::Null),
        &seed_settings,
        "heartbeat runtime must not apply onto desired runtime_settings: {}",
        after.body
    );

    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get host")
        .unwrap_or_else(|| panic!("host {host_id} missing after dirty heartbeat"));
    let reported = stored.runtime_reported.to_string();
    for key in ["extra_env", "machine_token", "FLEET_WORKER_TOKEN"] {
        assert!(
            stored.runtime_reported.get(key).is_none(),
            "stored runtime_reported must omit {key}: {}",
            stored.runtime_reported
        );
    }
    for needle in [unique_flm.as_str(), unique_secret.as_str(), "/tmp;id"] {
        assert!(
            !reported.contains(needle),
            "stored runtime_reported JSONB must omit {needle:?}: {reported}"
        );
    }
}

/// PATCH runtime_settings [] or a string is 400 and does not persist.
#[tokio::test]
async fn patch_runtime_settings_non_object_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let seed_settings = seed
        .body
        .get("runtime_settings")
        .cloned()
        .unwrap_or(Value::Null);

    for (label, body) in [
        ("array", json!({ "runtime_settings": [] })),
        ("string", json!({ "runtime_settings": "x" })),
    ] {
        let rejected = call(&app, "PATCH", &host_path, Some(&jar), Some(body)).await;
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "PATCH runtime_settings {label} must be 400: {}",
            rejected.body
        );
        let got = call(&app, "GET", &host_path, Some(&jar), None).await;
        assert_eq!(
            got.status,
            StatusCode::OK,
            "GET after {label}: {}",
            got.body
        );
        assert_eq!(
            got.body.get("runtime_settings").unwrap_or(&Value::Null),
            &seed_settings,
            "{label} reject must not persist runtime_settings: {}",
            got.body
        );
    }
}

/// PATCH work_dir with a newline or 513 chars is 400 and GET stays at seed.
#[tokio::test]
async fn patch_runtime_settings_rejects_control_and_overlong_work_dir() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "box", true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    let seed_settings = seed
        .body
        .get("runtime_settings")
        .cloned()
        .unwrap_or(Value::Null);

    let overlong = "x".repeat(513);
    assert_eq!(overlong.len(), 513, "overlong work_dir must be 513 chars");

    for (label, work_dir) in [
        ("newline", "/tmp/work\ncurl-evil".to_string()),
        ("513 chars", overlong),
    ] {
        let rejected = call(
            &app,
            "PATCH",
            &host_path,
            Some(&jar),
            Some(json!({ "runtime_settings": { "work_dir": work_dir } })),
        )
        .await;
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "PATCH work_dir {label} must be 400: {}",
            rejected.body
        );
        let got = call(&app, "GET", &host_path, Some(&jar), None).await;
        assert_eq!(
            got.status,
            StatusCode::OK,
            "GET after {label}: {}",
            got.body
        );
        assert_eq!(
            got.body.get("runtime_settings").unwrap_or(&Value::Null),
            &seed_settings,
            "{label} reject must leave runtime_settings at seed: {}",
            got.body
        );
    }
}

/// catalog:list-local-model-tags-rejects-unsafe-slug — member GET
/// `/local-models/{name}/tags` is 400 (not 200 `unavailable`) for an
/// unsafe slug; GET after reject stays 400; neither hop opens ollama.com
/// or :1234. Other-ws catalog/tags is 403 with no body. Unauth catalog
/// and tags is 401 (1128 deleted). Unauth unsafe slug is 401 not 400.
/// Same-ws member catalog/tags is not 403. Distinct from
/// `slug_rejects_paths` (helper bools) and disabled-no-proxy.
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

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
        // :1234 is the lab proxy honeypot this guard binds. :443 /proc
        // diffs race any HTTPS on the box (browser, ollama pull, telemetry)
        // and are not proof the handler dialed ollama.com — the 2s timeout
        // plus this honeypot already cover that.
        let new_1234: Vec<_> = tcp_rows_to_remote_port(1234)
            .difference(&self.before_1234)
            .cloned()
            .collect();
        assert!(
            new_1234.is_empty(),
            "must not open :1234 (lab proxy); new tcp rows {new_1234:?}"
        );
        let _ = &self.before_443;
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
async fn local_model_tags_rejects_unsafe_slug_is_400_not_unavailable() {
    let (app, _) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let path = local_model_tags_path(&ws, "hi;rm");

    let guard = LocalModelTagsNoOutbound::arm();
    const FAIL_CLOSED: Duration = Duration::from_secs(2);

    let first = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", &path, Some(&jar), None))
        .await
        .unwrap_or_else(|_| {
            panic!("unsafe slug first GET must 400 immediately, not open ollama.com/:1234")
        });
    assert_local_model_tags_400(&first, &format!("first {path}"));

    let again = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", &path, Some(&jar), None))
        .await
        .unwrap_or_else(|_| {
            panic!("unsafe slug second GET must stay 400 immediately, not open ollama.com/:1234")
        });
    assert_local_model_tags_400(&again, &format!("after reject {path}"));

    guard.assert_idle();
}

#[tokio::test]
async fn other_ws_local_models_catalog_and_tags_is_403() {
    let (app, _) = app_with_state().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let (_other_email, other_jar, other_ws) = signup_and_login(&app).await;
    assert_ne!(ws, other_ws, "A and B must have distinct workspaces");
    let catalog = local_models_catalog_path(&ws);
    let safe_tags = local_model_tags_path(&ws, "llama3.2");

    let guard = LocalModelTagsNoOutbound::arm();
    const FAIL_CLOSED: Duration = Duration::from_secs(2);

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

    guard.assert_idle();
}

#[tokio::test]
async fn unauth_local_models_catalog_and_tags_is_401() {
    let (app, _) = app_with_state().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let catalog = local_models_catalog_path(&ws);
    let safe_tags = local_model_tags_path(&ws, "llama3.2");

    let guard = LocalModelTagsNoOutbound::arm();
    const FAIL_CLOSED: Duration = Duration::from_secs(2);

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

    guard.assert_idle();
}

#[tokio::test]
async fn unauth_local_model_tags_unsafe_slug_is_401_not_400() {
    let (app, _) = app_with_state().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;
    let path = local_model_tags_path(&ws, "hi;rm");

    let guard = LocalModelTagsNoOutbound::arm();
    const FAIL_CLOSED: Duration = Duration::from_secs(2);

    let out = tokio::time::timeout(FAIL_CLOSED, call(&app, "GET", &path, None, None))
        .await
        .expect("unauth unsafe slug must 401 immediately, not open ollama.com/:1234");
    assert_ne!(
        out.status,
        StatusCode::BAD_REQUEST,
        "unauth GET /local-models/hi;rm/tags must not 400 before auth: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "unauth GET /local-models/hi;rm/tags must be 401: {}",
        out.body
    );

    guard.assert_idle();
}

#[tokio::test]
async fn same_ws_member_get_local_models_and_tags_is_not_403() {
    let (app, state) = app_with_state().await;
    let (_owner_email, _owner_jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");

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

    let catalog = call(
        &app,
        "GET",
        &local_models_catalog_path(&ws),
        Some(&member_jar),
        None,
    )
    .await;
    assert_ne!(
        catalog.status,
        StatusCode::FORBIDDEN,
        "same-ws member GET /local-models must not 403: {}",
        catalog.body
    );

    let tags = call(
        &app,
        "GET",
        &local_model_tags_path(&ws, "llama3.2"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_ne!(
        tags.status,
        StatusCode::FORBIDDEN,
        "same-ws member GET /local-models/llama3.2/tags must not 403: {}",
        tags.body
    );
    assert_ne!(
        tags.status,
        StatusCode::BAD_REQUEST,
        "same-ws member safe slug must not 400: {}",
        tags.body
    );
}

/// catalog:list-max-job-timeout-below-60-400 — owner PATCH
/// max_job_timeout_s=0 is 400 (not clamp-and-200, not a SQL CHECK 500).
/// GET stays default 3600. Persist 60 and bundled timeout+concurrent
/// are sibling tests. Product failure: a 0s cap is stored.
#[tokio::test]
async fn max_job_timeout_below_60_is_400_and_stored_value_unchanged() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("timeout-{}", Uuid::new_v4());
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    assert_eq!(
        seed.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "paired host default max_job_timeout_s is 3600: {}",
        seed.body
    );

    let rejected = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_job_timeout_s": 0 })),
    )
    .await;
    assert_ne!(
        rejected.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "max_job_timeout_s=0 must not 500 from the SQL CHECK: {}",
        rejected.body
    );
    assert_ne!(
        rejected.status,
        StatusCode::OK,
        "max_job_timeout_s=0 must not clamp-and-200: {}",
        rejected.body
    );
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "max_job_timeout_s=0 must be 400: {}",
        rejected.body
    );

    let leftover = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "GET after reject 0: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "GET after reject still default 3600 (0s cap must not persist): {}",
        leftover.body
    );
}

/// Owner PATCH max_job_timeout_s=60 is 200; GET and list show 60, not 3600.
#[tokio::test]
async fn max_job_timeout_patch_60_persists_on_get_and_list() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("timeout-persist-{}", Uuid::new_v4());
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    let hid = host_id.to_string();
    let host_path = format!("{}/{hid}", hosts_path(&ws));

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
        "PATCH 60 must persist: {}",
        persist.body
    );
    assert_eq!(
        persist.body["max_job_timeout_s"].as_i64(),
        Some(60),
        "PATCH 60 must store 60 (not stay at 3600): {}",
        persist.body
    );

    let leftover = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "GET after persist 60: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["max_job_timeout_s"].as_i64(),
        Some(60),
        "GET after persist 60 must be 60: {}",
        leftover.body
    );

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
}

/// PATCH {max_job_timeout_s:0, max_concurrent:1} is 400; GET stays 3600/4.
#[tokio::test]
async fn patch_timeout_below_60_bundled_with_max_concurrent_writes_neither() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let name = format!("timeout-bundled-{}", Uuid::new_v4());
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, &name, true, false).await;
    let host_path = format!("{}/{host_id}", hosts_path(&ws));

    let seed = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(seed.status, StatusCode::OK, "GET seed: {}", seed.body);
    assert_eq!(
        seed.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "paired host default max_job_timeout_s is 3600: {}",
        seed.body
    );
    assert_eq!(
        seed.body["max_concurrent"].as_i64(),
        Some(4),
        "paired host default max_concurrent is 4: {}",
        seed.body
    );

    let rejected = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_job_timeout_s": 0, "max_concurrent": 1 })),
    )
    .await;
    assert_ne!(
        rejected.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "bundled timeout 0 must not 500: {}",
        rejected.body
    );
    assert_ne!(
        rejected.status,
        StatusCode::OK,
        "bundled timeout 0 must not write max_concurrent=1: {}",
        rejected.body
    );
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "bundled timeout 0 must be 400: {}",
        rejected.body
    );

    let leftover = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "GET after bundled reject: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["max_job_timeout_s"].as_i64(),
        Some(3600),
        "GET after bundled reject still default 3600: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["max_concurrent"].as_i64(),
        Some(4),
        "GET after bundled reject still default max_concurrent=4: {}",
        leftover.body
    );
}

#[tokio::test]
async fn llm_keys_create_list_never_returns_secret() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let path = format!("/api/workspaces/{ws}/llm-keys");
    let created = call(
        &app,
        "POST",
        &path,
        Some(&jar),
        Some(json!({
            "provider": "grok",
            "secret": "sk-test-never-echo-this-value"
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create key: {}",
        created.body
    );
    let hay = format!("{} {}", created.body, created.raw);
    assert!(
        !hay.contains("sk-test-never-echo-this-value"),
        "create must not echo the secret: {hay}"
    );
    assert_eq!(created.body["key"]["provider"], "grok");
    assert_eq!(created.body["key"]["revoked"], false);

    let listed = call(&app, "GET", &path, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let list_hay = format!("{} {}", listed.body, listed.raw);
    assert!(
        !list_hay.contains("sk-test-never-echo-this-value"),
        "list must not include the secret: {list_hay}"
    );
    let keys = listed.body["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1, "{}", listed.body);
    assert_eq!(keys[0]["provider"], "grok");
}

#[tokio::test]
async fn llm_keys_revoke_hides_key_from_list() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let path = format!("/api/workspaces/{ws}/llm-keys");
    let created = call(
        &app,
        "POST",
        &path,
        Some(&jar),
        Some(json!({ "provider": "claude", "secret": "claude-secret-value" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["key"]["id"].as_str().expect("id").to_string();
    let revoked = call(
        &app,
        "POST",
        &format!("{path}/{id}/revoke"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "revoke: {}", revoked.body);
    let listed = call(&app, "GET", &path, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let keys = listed.body["keys"].as_array().expect("keys");
    assert!(
        keys.is_empty(),
        "revoked key must leave the active list: {}",
        listed.body
    );
}

#[tokio::test]
async fn fixture_run_mode_in_container_is_stamped() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let _ = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/fleet"),
        Some(&jar),
        None,
    )
    .await;
    let fx = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/fleet/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain", "run_mode": "in_container" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);
    let plan_id = Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("uuid");
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let plan = state
        .orch
        .store
        .get_plan_body(plan_id)
        .await
        .expect("get plan")
        .expect("plan");
    assert_eq!(plan.project_id, ws_id);
    assert_eq!(
        plan.policy.run_mode, "in_container",
        "fixture run_mode must stamp the plan: {:?}",
        plan.policy
    );
}
