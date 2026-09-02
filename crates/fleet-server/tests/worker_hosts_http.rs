//! HTTP tests for paired worker hosts and public install scripts.
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use fleet_auth::{ACCESS_COOKIE, AuthConfig, REFRESH_COOKIE};
use fleet_db::{PoolConfig, connect_and_migrate_with};
use fleet_integrations::{CredentialsKey, IntegrationsConfig};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{
    AppState, ClaudeConfig, GrokConfig, OllamaConfig, OpenaiConfig, ServerConfig, build_http_app,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
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

/// In-process app + state (copied from http_api.rs; do not import private items).
async fn app_with_state() -> (axum::Router, AppState) {
    app_with_state_dist(None).await
}

async fn app_with_state_dist(worker_dist_dir: Option<PathBuf>) -> (axum::Router, AppState) {
    if std::env::var("FLEET_BOOTSTRAP_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_none()
    {
        // SAFETY: this integration binary is the only writer; tests share one secret.
        unsafe {
            std::env::set_var("FLEET_BOOTSTRAP_SECRET", TEST_BOOTSTRAP_SECRET);
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
    let config = ServerConfig {
        database_url: database_url.clone(),
        bind: "127.0.0.1:0".parse().expect("bind"),
        cors_origins: vec![
            "http://localhost:3030".into(),
            "http://127.0.0.1:3030".into(),
        ],
        auth,
        integrations: test_integrations_config(&jwt_secret),
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
    assert_eq!(blank.status, StatusCode::OK, "blank: {}", blank.body);
    assert_eq!(
        blank.body["name"], "renamed-studio",
        "blank PATCH name must not wipe: {}",
        blank.body
    );
    assert_eq!(
        blank.body["settings_gen"].as_i64().expect("blank gen"),
        gen1,
        "blank PATCH name must not bump gen: {}",
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
    .map(|h| h.id)
    .collect()
}

#[tokio::test]
async fn pair_once_second_pair_rejected() {
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
    let leftover = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid2.as_str()))
        .expect("leftover pending");
    assert_eq!(leftover["status"], "disabled");
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

#[tokio::test]
async fn install_worker_bin_missing_404() {
    let app = app().await;
    let out = call(
        &app,
        "GET",
        "/install/worker/bin/no-such-os/fleet-host",
        None,
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.raw);
    let err = out.body["error"].as_str().unwrap_or(&out.raw);
    assert!(
        err.contains("FLEET_WORKER_DIST_DIR") || err.contains("binary not available"),
        "{err}"
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

#[tokio::test]
async fn bootstrap_pairs_and_rotates_same_machine() {
    let app = app().await;
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

#[tokio::test]
async fn local_models_requires_auth() {
    let (app, _) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let path = format!("/api/workspaces/{ws}/local-models");
    let anon = call(&app, "GET", &path, None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{}", anon.body);
    // Do not assert the 200 catalog body here: catalog() may hit ollama.com.
    let _ = jar;
}
