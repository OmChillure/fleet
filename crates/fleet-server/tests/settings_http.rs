//! HTTP: GET/PATCH fleet settings `min_isolation`.
//! Requires DATABASE_URL — never skips.
//!
//! Helpers are copied from `http_api.rs` (that file is not modified).

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

/// In-process app + state (copied from http_api.rs; do not import private items).
async fn app_with_state() -> (axum::Router, AppState) {
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
        gemini: GeminiConfig {
            enabled: true,
            api_key: None,
            api_base: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
        },
        worker_dist_dir: None,
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

fn fleet(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/fleet")
    } else {
        format!("/api/workspaces/{ws}/fleet{suffix}")
    }
}

#[tokio::test]
async fn fleet_settings_get_min_isolation_defaults_container() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let got = call(&app, "GET", &fleet(&ws, "/settings"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["min_isolation"], "container",
        "new workspace default min_isolation: {}",
        got.body
    );
}

#[tokio::test]
async fn fleet_settings_egress_defaults_inherit_and_owner_can_open() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let got = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["default_egress"], "inherit", "{}", got.body);
    assert_eq!(got.body["default_egress_allowlist"], "", "{}", got.body);

    let patched = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({
            "default_egress": "allowlist",
            "default_egress_allowlist": "10.0.0.0/8, 172.18.0.1/32"
        })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(
        patched.body["default_egress"], "allowlist",
        "{}",
        patched.body
    );
    assert_eq!(
        patched.body["default_egress_allowlist"], "10.0.0.0/8, 172.18.0.1/32",
        "{}",
        patched.body
    );

    let bad = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "default_egress": "package_registries" })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::CONFLICT, "{}", bad.body);
}

#[tokio::test]
async fn repo_posture_is_saved_and_envelope_defaults_wide() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let got = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["run_envelope"], "on_pc", "{}", got.body);
    assert_eq!(got.body["egress_envelope"], "open", "{}", got.body);

    let capped = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "run_envelope": "in_container", "egress_envelope": "none" })),
    )
    .await;
    assert_eq!(capped.status, StatusCode::OK, "{}", capped.body);
    assert_eq!(
        capped.body["run_envelope"], "in_container",
        "{}",
        capped.body
    );
    assert_eq!(capped.body["egress_envelope"], "none", "{}", capped.body);

    let saved = call(
        &app,
        "PUT",
        &fleet(&ws, "/repo-postures"),
        Some(&jar),
        Some(json!({
            "repo": "acme/app",
            "run_mode": "in_container",
            "egress_mode": "none"
        })),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["repo"], "acme/app", "{}", saved.body);

    let listed = call(&app, "GET", &fleet(&ws, "/repo-postures"), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["repos"][0]["repo"], "acme/app",
        "{}",
        listed.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_min_isolation_container_persists() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({ "min_isolation": "container" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(
        patched.body["min_isolation"], "container",
        "{}",
        patched.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_min_isolation_native_is_409() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "min_isolation": "container" })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let out = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "min_isolation": "native" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CONFLICT, "{}", out.body);
    assert!(
        out.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "409 body must include error: {}",
        out.body
    );

    let after = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["min_isolation"], "container",
        "rejected PATCH must not change stored min_isolation: {}",
        after.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_min_isolation_vm_persists() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let patched = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "min_isolation": "vm" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["min_isolation"], "vm", "{}", patched.body);

    let got = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["min_isolation"], "vm", "{}", got.body);
}

#[tokio::test]
async fn fleet_settings_patch_omitted_min_isolation_preserves_container() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "min_isolation": "container" })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let patched = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 4 })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["max_agents"], 4, "{}", patched.body);
    assert_eq!(
        patched.body["min_isolation"], "container",
        "omitted min_isolation must keep container: {}",
        patched.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_min_isolation_without_cookie_is_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let out = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        None,
        Some(json!({ "min_isolation": "container" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
}

#[tokio::test]
async fn fleet_settings_chat_max_turns_defaults_32_and_patches() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let got = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["chat_max_turns"], 32, "{}", got.body);
    assert_eq!(got.body["chat_max_turns_min"], 1, "{}", got.body);
    assert_eq!(got.body["chat_max_turns_max"], 128, "{}", got.body);

    let patched = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "chat_max_turns": 8 })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["chat_max_turns"], 8, "{}", patched.body);

    let keep = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 4 })),
    )
    .await;
    assert_eq!(
        keep.body["chat_max_turns"], 8,
        "omitted chat_max_turns must keep 8: {}",
        keep.body
    );

    let bad = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "chat_max_turns": 0 })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    let after = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(
        after.body["chat_max_turns"], 8,
        "rejected chat_max_turns must not store: {}",
        after.body
    );
}
