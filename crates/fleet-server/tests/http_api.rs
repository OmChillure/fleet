//! Fail-closed HTTP integration tests for fleet-server (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use fleet_auth::{AuthConfig, ACCESS_COOKIE, REFRESH_COOKIE};
use fleet_db::{connect_and_migrate_with, PoolConfig};
use fleet_integrations::{CredentialsKey, IntegrationsConfig};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{
    build_http_app, AppState, ClaudeConfig, GeminiConfig, GrokConfig, OllamaConfig, OpenaiConfig, ServerConfig,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_PASSWORD: &str = "correct-horse";
const TEST_ADMIN_TOKEN: &str = "test-admin";
const TEST_JWT_SECRET: &str = "dev-change-me-to-a-long-random-secret-at-least-32-chars";
const TEST_WEBHOOK_SECRET: &str = "test-webhook-secret";

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

/// Inbox JSON exposes the token in `hook_url`, not a separate `hook_token` field.
fn hook_token_from_monitor(body: &Value) -> String {
    if let Some(t) = body["hook_token"].as_str().filter(|s| !s.is_empty()) {
        return t.to_string();
    }
    body["hook_url"]
        .as_str()
        .and_then(|u| u.rsplit('/').next())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("hook_url token: {body}"))
        .to_string()
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

/// In-process app + state (state is for direct tool calls without an LLM).
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

async fn call_raw(
    app: &axum::Router,
    method: &str,
    uri: &str,
    extra: &[(&str, &str)],
    body: Vec<u8>,
) -> HttpOut {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    if !extra
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let req = builder.body(Body::from(body)).unwrap();
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

fn fleet(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/fleet")
    } else {
        format!("/api/workspaces/{ws}/fleet{suffix}")
    }
}

fn partners_disclaimer(ws: &str) -> String {
    format!("/api/workspaces/{ws}/partners/disclaimer")
}

fn this_machine_hostname() -> String {
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
        .unwrap_or_else(|| "test-box".into())
}

async fn pair_test_host(app: &axum::Router, jar: &CookieJar, ws: &str) -> String {
    let created = call(
        app,
        "POST",
        &format!("/api/workspaces/{ws}/worker-hosts"),
        Some(jar),
        Some(json!({ "name": "test-host" })),
    )
    .await;
    assert!(
        created.status == StatusCode::OK || created.status == StatusCode::CREATED,
        "create host: {} {}",
        created.status,
        created.body
    );
    let token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair_token: {}", created.body))
        .to_string();
    let hostname = this_machine_hostname();
    let paired = call(
        app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": token,
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let machine = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("machine_token: {}", paired.body))
        .to_string();
    let hb = call_bearer(
        app,
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
    machine
}

async fn drain_leases(app: &axum::Router, machine: &str, ws: &str) {
    for _ in 0..8 {
        let lease = call_bearer(
            app,
            "POST",
            "/api/worker-hosts/me/lease",
            machine,
            Some(json!({})),
        )
        .await;
        if lease.status != StatusCode::OK {
            return;
        }
        let claim = &lease.body["claim"];
        if claim.is_null() {
            return;
        }
        let id = claim["id"].as_str().unwrap_or("");
        let tok = claim["lease_token"].as_str().unwrap_or("");
        if id.is_empty() {
            return;
        }
        let session_id = claim["payload"]["session_id"].as_str().unwrap_or("");
        let worker_token = claim["payload"]["worker_token"].as_str().unwrap_or("");
        if !session_id.is_empty() && !worker_token.is_empty() {
            let _ = call_bearer(
                app,
                "POST",
                &format!("/v1/projects/{ws}/workers/{session_id}/status"),
                worker_token,
                Some(json!({
                    "seq": 1,
                    "status": "success",
                    "result_md": "Task completed successfully with artifacts.",
                    "files": { "work/out.txt": "ok" }
                })),
            )
            .await;
        }
        let _ = call_bearer(
            app,
            "POST",
            &format!("/api/worker-hosts/me/lease/{id}/complete"),
            machine,
            Some(json!({ "lease_token": tok, "ok": true })),
        )
        .await;
    }
}

async fn tick_until_tasks_done(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    want: &[&str],
) -> Value {
    let machine = pair_test_host(app, jar, ws).await;
    let mut last_tasks = json!({});
    for i in 0..24 {
        let tick = call(app, "POST", &fleet(ws, "/tick"), Some(jar), None).await;
        assert_eq!(tick.status, StatusCode::OK, "tick {i}: {}", tick.body);
        drain_leases(app, &machine, ws).await;
        tokio::time::sleep(Duration::from_millis(75)).await;
        let tasks = call(app, "GET", &fleet(ws, "/tasks"), Some(jar), None).await;
        assert_eq!(tasks.status, StatusCode::OK, "tasks {i}: {}", tasks.body);
        last_tasks = tasks.body;
        let list = last_tasks["tasks"].as_array().cloned().unwrap_or_default();
        let done = want.iter().all(|id| {
            list.iter()
                .any(|t| t["plan_task_id"].as_str() == Some(*id) && t["status"] == "done_success")
        });
        if done && !list.is_empty() {
            return last_tasks;
        }
    }
    panic!("tasks did not reach done_success for {want:?} within 24 ticks: {last_tasks}");
}

// ── 1. health / healthz ─────────────────────────────────────────────────────

#[tokio::test]
async fn health_ok_healthz_ok_api_index_lists_auth_and_fleet() {
    let app = app().await;

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK, "{}", health.body);
    assert_eq!(health.body["status"], "ok");
    assert_eq!(health.body["service"], "fleet-server");
    assert_eq!(health.body["db"], "up");
    assert!(
        health.body["apis"].is_array(),
        "health must advertise apis as an array: {}",
        health.body
    );
    assert!(
        health.body.get("git_sha").is_some(),
        "health must expose git_sha (null when FLEET_GIT_SHA unset at compile): {}",
        health.body
    );
}

#[tokio::test]
async fn health_degraded_200_when_pool_cannot_acquire() {
    let (app, state) = app_with_state().await;
    state.db.close().await;

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 when the pool cannot acquire: {}",
        health.body
    );
    assert_eq!(health.body["status"], "degraded");
    assert_eq!(health.body["db"], "down");
}

#[tokio::test]
async fn healthz_fallback_ok_without_db_on_unified_app() {
    let (app, state) = app_with_state().await;
    state.orch.store.pool().close().await;

    let healthz = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(healthz.status, StatusCode::OK, "{}", healthz.body);
    assert_eq!(healthz.body["status"], "ok");
    assert!(
        healthz.body.get("db").is_none(),
        "fallback /healthz must not acquire or report DB: {}",
        healthz.body
    );
}

#[tokio::test]
async fn fleet_server_fallback_readyz_503_when_orch_pool_cannot_acquire() {
    let (app, state) = app_with_state().await;
    state.orch.store.pool().close().await;

    let out = call(&app, "GET", "/readyz", None, None).await;
    assert_eq!(
        out.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "fallback /readyz must be 503 when orch pool cannot acquire: {}",
        out.body
    );
}

// ── 2. signup/login → fleet project ─────────────────────────────────────────

#[tokio::test]
async fn signup_login_fleet_project_id_matches_workspace_owner() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fleet = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(fleet.status, StatusCode::OK, "{}", fleet.body);
    assert_eq!(
        fleet.body["project_id"].as_str(),
        Some(ws.as_str()),
        "project_id == workspace_id: {}",
        fleet.body
    );
    assert_eq!(fleet.body["workspace_id"].as_str(), Some(ws.as_str()));
    assert_eq!(fleet.body["role"], "owner");
}

// ── 3. cookie BFF Start 2-task + Run tick (orch TickRunner lives in fleet-orchestrator) ─

#[tokio::test]
async fn fixture_bff_start_and_tick_with_cookie() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    assert_eq!(fx.body["kind"], "chain", "{}", fx.body);
    assert_eq!(fx.body["project_id"].as_str(), Some(ws.as_str()));
    assert_eq!(fx.body["status"], "accepted", "{}", fx.body);
    assert!(fx.body["run_id"].as_str().is_some(), "{}", fx.body);
    assert!(fx.body["plan_id"].as_str().is_some(), "{}", fx.body);
}

#[tokio::test]
async fn fixture_does_not_materialize_until_tick() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);

    let before = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let before_list = before.body["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        !before_list
            .iter()
            .any(|t| t["plan_task_id"].as_str() == Some("t1")
                || t["plan_task_id"].as_str() == Some("t2")),
        "Start 2-task must not materialize t1/t2 before tick: {}",
        before.body
    );

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    assert!(
        tick.body["plans_materialized"].as_u64().unwrap_or(0) >= 1,
        "tick must report plans_materialized: {}",
        tick.body
    );
    assert!(
        tick.body["tasks_materialized"].as_u64().unwrap_or(0) >= 1,
        "tick must report tasks_materialized: {}",
        tick.body
    );

    let after = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    let after_list = after.body["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        after_list
            .iter()
            .any(|t| t["plan_task_id"].as_str() == Some("t1")),
        "expected plan_task_id t1 after tick: {}",
        after.body
    );
    assert!(
        after_list
            .iter()
            .any(|t| t["plan_task_id"].as_str() == Some("t2")),
        "expected plan_task_id t2 after tick: {}",
        after.body
    );
}

#[tokio::test]
async fn fixture_and_tick_without_cookie_are_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        None,
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::UNAUTHORIZED, "{}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), None, None).await;
    assert_eq!(tick.status, StatusCode::UNAUTHORIZED, "{}", tick.body);
}

#[tokio::test]
async fn fixture_unknown_kind_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "not-a-kind" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::BAD_REQUEST, "{}", fx.body);
    assert!(
        fx.body["error"]
            .as_str()
            .unwrap_or("")
            .contains("unknown fixture kind"),
        "unknown kind must fail closed: {}",
        fx.body
    );
}

#[tokio::test]
async fn resume_non_blocked_task_is_conflict() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let task = &tasks.body["tasks"][0];
    let task_id = task["id"].as_str().expect("task id");
    assert_ne!(
        task["status"].as_str(),
        Some("blocked"),
        "fixture card must not already be blocked: {}",
        tasks.body
    );

    let resume = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/tasks/{task_id}/resume")),
        Some(&jar),
        Some(json!({ "message": "go" })),
    )
    .await;
    assert_eq!(resume.status, StatusCode::CONFLICT, "{}", resume.body);
    assert!(
        resume.body["error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "409 must publish ErrorBody.error: {}",
        resume.body
    );
}

#[tokio::test]
async fn unknown_session_kill_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let missing = Uuid::new_v4();
    let kill = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/sessions/{missing}/kill")),
        Some(&jar),
        Some(json!({ "reason": "test" })),
    )
    .await;
    assert_eq!(kill.status, StatusCode::NOT_FOUND, "{}", kill.body);
    assert!(
        kill.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "404 must publish ErrorBody.error: {}",
        kill.body
    );
}

#[tokio::test]
async fn resume_blocked_task_returns_200_and_echoes_message() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let task_id = Uuid::parse_str(tasks.body["tasks"][0]["id"].as_str().expect("task id"))
        .expect("task uuid");
    let planted = sqlx::query("UPDATE board_tasks SET status = 'blocked' WHERE id = $1")
        .bind(task_id)
        .execute(state.orch.store.pool())
        .await
        .expect("plant blocked");
    assert_eq!(planted.rows_affected(), 1, "plant status=blocked");

    let resume = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/tasks/{task_id}/resume")),
        Some(&jar),
        Some(json!({ "message": "approved by operator" })),
    )
    .await;
    assert_eq!(resume.status, StatusCode::OK, "{}", resume.body);
    assert_eq!(resume.body["resume_requested"], true, "{}", resume.body);
    assert_eq!(
        resume.body["message"].as_str(),
        Some("approved by operator"),
        "{}",
        resume.body
    );
}

async fn plant_remote_session(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
) -> Uuid {
    use fleet_orchestrator::sandbox::remote;
    use fleet_orchestrator::store::NewWorkerSession;

    let fx = call(
        app,
        "POST",
        &fleet(ws, "/runs/fixture"),
        Some(jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let run_id = Uuid::parse_str(fx.body["run_id"].as_str().expect("run_id")).expect("run uuid");
    let tick = call(app, "POST", &fleet(ws, "/tick"), Some(jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(app, "GET", &fleet(ws, "/tasks"), Some(jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let task_id = Uuid::parse_str(tasks.body["tasks"][0]["id"].as_str().expect("task id"))
        .expect("task uuid");
    // Tick may have spawned attempt 1 onto a leftover assignable host.
    sqlx::query("DELETE FROM worker_sessions WHERE board_task_id = $1")
        .bind(task_id)
        .execute(state.orch.store.pool())
        .await
        .expect("clear tick-spawned session for this task");
    let ws_id = Uuid::parse_str(ws).expect("ws");
    let sid = Uuid::new_v4();
    state
        .orch
        .store
        .insert_session_spawning(&NewWorkerSession {
            id: sid,
            project_id: ws_id,
            run_id,
            board_task_id: task_id,
            attempt: 1,
            runtime_backend: remote::BACKEND.into(),
            worker_token: format!("kill-{sid}"),
            worker_profile: "generalist".into(),
            work_dir: None,
        })
        .await
        .expect("plant remote session");
    state
        .orch
        .store
        .record_remote_enqueued(sid, &remote::runtime_handle(Uuid::new_v4()), None)
        .await
        .expect("plant runtime_handle");
    sid
}

#[tokio::test]
async fn post_kill_live_remote_session_returns_200_killed_and_signaled() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let sid = plant_remote_session(&app, &state, &jar, &ws).await;

    let kill = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/sessions/{sid}/kill")),
        Some(&jar),
        Some(json!({ "reason": "test" })),
    )
    .await;
    assert_eq!(kill.status, StatusCode::OK, "{}", kill.body);
    assert_eq!(kill.body["killed"], true, "{}", kill.body);
    assert_eq!(kill.body["signaled"], true, "{}", kill.body);
}

#[tokio::test]
async fn kill_session_other_workspace_path_is_404() {
    let (app, state) = app_with_state().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must be distinct workspaces");
    let sid = plant_remote_session(&app, &state, &a_jar, &a_ws).await;

    let kill = call(
        &app,
        "POST",
        &fleet(&b_ws, &format!("/sessions/{sid}/kill")),
        Some(&b_jar),
        Some(json!({ "reason": "test" })),
    )
    .await;
    assert_eq!(kill.status, StatusCode::NOT_FOUND, "{}", kill.body);
    assert!(
        kill.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "cross-workspace kill must publish ErrorBody.error: {}",
        kill.body
    );
}

// ── 4. cancel run ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_run_reaches_cancelled() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let run_id = fx.body["run_id"].as_str().expect("run_id").to_string();

    let cancel = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/runs/{run_id}/cancel")),
        Some(&jar),
        Some(json!({ "reason": "http_api cancel" })),
    )
    .await;
    assert_eq!(cancel.status, StatusCode::OK, "{}", cancel.body);
    assert_eq!(cancel.body["cancel_requested"], true, "{}", cancel.body);
    assert_eq!(
        cancel.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "{}",
        cancel.body
    );
    assert_eq!(
        cancel.body["reason"].as_str(),
        Some("http_api cancel"),
        "{}",
        cancel.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    let run = runs.body["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["id"].as_str() == Some(run_id.as_str()))
        .unwrap_or_else(|| panic!("run {run_id} missing from GET /runs: {}", runs.body));
    assert!(
        run["cancel_requested_at"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "cancel_requested_at must be set: {}",
        run
    );
}

#[tokio::test]
async fn cancel_unknown_run_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let missing = Uuid::new_v4();
    let cancel = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/runs/{missing}/cancel")),
        Some(&jar),
        Some(json!({ "reason": "http_api cancel" })),
    )
    .await;
    assert_eq!(cancel.status, StatusCode::BAD_REQUEST, "{}", cancel.body);
    assert!(
        cancel.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "400 must publish ErrorBody.error: {}",
        cancel.body
    );
}

#[tokio::test]
async fn cancel_other_workspace_is_403() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let run_id = fx.body["run_id"].as_str().expect("run_id").to_string();

    let cancel = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/runs/{run_id}/cancel")),
        Some(&b_jar),
        Some(json!({ "reason": "http_api cancel" })),
    )
    .await;
    assert_eq!(cancel.status, StatusCode::FORBIDDEN, "{}", cancel.body);
}

#[tokio::test]
async fn cancel_without_cookie_is_401() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let run_id = fx.body["run_id"].as_str().expect("run_id").to_string();

    let cancel = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/runs/{run_id}/cancel")),
        None,
        Some(json!({ "reason": "http_api cancel" })),
    )
    .await;
    assert_eq!(cancel.status, StatusCode::UNAUTHORIZED, "{}", cancel.body);
}

// ── 5. PATCH fleet config ───────────────────────────────────────────────────

#[tokio::test]
async fn patch_fleet_config_sets_repo_url_and_git_ref() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({
            "repo_url": "https://github.com/acme/app.git",
            "git_ref": "develop"
        })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(
        patched.body["config"]["repo_url"],
        "https://github.com/acme/app.git",
        "{}",
        patched.body
    );
    assert_eq!(
        patched.body["config"]["git_ref"],
        "develop",
        "{}",
        patched.body
    );

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["config"]["repo_url"],
        "https://github.com/acme/app.git",
        "{}",
        got.body
    );
    assert_eq!(got.body["config"]["git_ref"], "develop", "{}", got.body);
}

#[tokio::test]
async fn patch_fleet_config_empty_repo_url_clears() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let bound = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({
            "repo_url": "https://github.com/acme/app.git",
            "git_ref": "develop"
        })),
    )
    .await;
    assert_eq!(bound.status, StatusCode::OK, "{}", bound.body);

    let cleared = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({ "repo_url": "" })),
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert!(
        cleared.body["config"]["repo_url"].is_null(),
        "empty repo_url should clear: {}",
        cleared.body
    );
    assert_eq!(
        cleared.body["config"]["git_ref"],
        "main",
        "clear must reset git_ref: {}",
        cleared.body
    );

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert!(
        got.body["config"]["repo_url"].is_null(),
        "GET after clear must show no remote: {}",
        got.body
    );
    assert_eq!(
        got.body["config"]["git_ref"],
        "main",
        "GET after clear must reset git_ref: {}",
        got.body
    );
}

#[tokio::test]
async fn patch_fleet_config_git_ref_only_without_url_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let primed = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(primed.status, StatusCode::OK, "{}", primed.body);
    assert!(
        primed.body["config"]["repo_url"].is_null(),
        "fresh workspace must have no remote: {}",
        primed.body
    );

    let ref_only = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(
        ref_only.status,
        StatusCode::BAD_REQUEST,
        "{}",
        ref_only.body
    );
    assert!(
        ref_only.body["error"]
            .as_str()
            .unwrap_or("")
            .contains("repo_url"),
        "400 must mention repo_url: {}",
        ref_only.body
    );

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert!(
        got.body["config"]["repo_url"].is_null(),
        "ref-only 400 must not bind a remote: {}",
        got.body
    );
    assert_eq!(
        got.body["config"]["git_ref"],
        "main",
        "ref-only 400 must not change git_ref: {}",
        got.body
    );
}

#[tokio::test]
async fn patch_fleet_config_omitted_url_updates_git_ref_when_remote_bound() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let bound = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({
            "repo_url": "https://github.com/acme/app.git",
            "git_ref": "develop"
        })),
    )
    .await;
    assert_eq!(bound.status, StatusCode::OK, "{}", bound.body);

    let patched = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(
        patched.body["config"]["repo_url"],
        "https://github.com/acme/app.git",
        "{}",
        patched.body
    );
    assert_eq!(
        patched.body["config"]["git_ref"],
        "feature",
        "{}",
        patched.body
    );

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["config"]["repo_url"],
        "https://github.com/acme/app.git",
        "{}",
        got.body
    );
    assert_eq!(got.body["config"]["git_ref"], "feature", "{}", got.body);
}

#[tokio::test]
async fn patch_fleet_config_invalid_url_is_400_and_leaves_remote() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let bound = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({
            "repo_url": "https://github.com/acme/app.git",
            "git_ref": "develop"
        })),
    )
    .await;
    assert_eq!(bound.status, StatusCode::OK, "{}", bound.body);

    let bad = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({ "repo_url": "not a url" })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    assert!(
        bad.body["error"]
            .as_str()
            .unwrap_or("")
            .contains("repo_url"),
        "400 must mention repo_url: {}",
        bad.body
    );

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["config"]["repo_url"],
        "https://github.com/acme/app.git",
        "invalid URL must leave the remote: {}",
        got.body
    );
    assert_eq!(
        got.body["config"]["git_ref"],
        "develop",
        "invalid URL must leave git_ref: {}",
        got.body
    );
}

#[tokio::test]
async fn patch_fleet_config_other_workspace_cookie_is_403() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &fleet(&a_ws, "/config"),
        Some(&b_jar),
        Some(json!({
            "repo_url": "https://github.com/acme/app.git",
            "git_ref": "develop"
        })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);
}

// ── 6. workers / sessions list + detail ─────────────────────────────────────

#[tokio::test]
async fn workers_sandboxes_match_sessions_ids_and_fields() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let sid = plant_remote_session(&app, &state, &jar, &ws).await;

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let session_list = sessions.body["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("sessions array: {}", sessions.body));
    assert_eq!(
        session_list.len(),
        1,
        "planted row only: {}",
        sessions.body
    );
    let session = &session_list[0];
    assert_eq!(session["id"], sid.to_string(), "{}", sessions.body);
    assert_eq!(session["status"], "spawning", "{}", sessions.body);
    assert_eq!(session["runtime_backend"], "remote", "{}", sessions.body);
    assert_eq!(session["worker_profile"], "generalist", "{}", sessions.body);
    assert_eq!(session["attempt"], 1, "{}", sessions.body);

    let workers = call(&app, "GET", &fleet(&ws, "/workers"), Some(&jar), None).await;
    assert_eq!(workers.status, StatusCode::OK, "{}", workers.body);
    let sandboxes = workers.body["sandboxes"]
        .as_array()
        .unwrap_or_else(|| panic!("sandboxes array: {}", workers.body));
    assert_eq!(sandboxes.len(), 1, "planted row only: {}", workers.body);
    let sandbox = &sandboxes[0];
    assert_eq!(sandbox["id"], sid.to_string(), "{}", workers.body);
    assert_eq!(sandbox["id"], session["id"], "{}", workers.body);
    assert_eq!(sandbox["run_id"], session["run_id"], "{}", workers.body);
    assert_eq!(
        sandbox["board_task_id"], session["board_task_id"],
        "{}",
        workers.body
    );
    assert_eq!(sandbox["attempt"], session["attempt"], "{}", workers.body);
    assert_eq!(sandbox["status"], session["status"], "{}", workers.body);
    assert_eq!(
        sandbox["runtime_backend"], session["runtime_backend"],
        "{}",
        workers.body
    );
    assert_eq!(
        sandbox["worker_profile"], session["worker_profile"],
        "{}",
        workers.body
    );
    assert_eq!(
        sandbox["progress_pct"], session["progress_pct"],
        "{}",
        workers.body
    );
}

#[tokio::test]
async fn get_session_detail_shape_and_unknown_id_is_404() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let sid = plant_remote_session(&app, &state, &jar, &ws).await;

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    assert_eq!(
        sessions.body["sessions"][0]["id"],
        sid.to_string(),
        "{}",
        sessions.body
    );
    let run_id = sessions.body["sessions"][0]["run_id"].clone();
    let status = sessions.body["sessions"][0]["status"].clone();

    let detail = call(
        &app,
        "GET",
        &fleet(&ws, &format!("/sessions/{sid}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(
        detail.body["session"]["id"],
        sid.to_string(),
        "{}",
        detail.body
    );
    assert_eq!(detail.body["session"]["run_id"], run_id, "{}", detail.body);
    assert_eq!(detail.body["session"]["status"], status, "{}", detail.body);
    assert!(
        detail.body["prompt"].is_string(),
        "prompt must be a string: {}",
        detail.body
    );
    assert!(
        detail.body["live"].is_array(),
        "live must be an array: {}",
        detail.body
    );
    assert!(
        detail.body["transcript"].is_string(),
        "transcript must be a string: {}",
        detail.body
    );
    assert!(
        detail.body.get("result").is_some(),
        "result key: {}",
        detail.body
    );

    let missing = call(
        &app,
        "GET",
        &fleet(&ws, &format!("/sessions/{}", Uuid::new_v4())),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
    assert!(
        missing.body["error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "404 must publish ErrorBody.error: {}",
        missing.body
    );
}

#[tokio::test]
async fn get_workers_and_sessions_without_cookie_are_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let workers = call(&app, "GET", &fleet(&ws, "/workers"), None, None).await;
    assert_eq!(workers.status, StatusCode::UNAUTHORIZED, "{}", workers.body);

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), None, None).await;
    assert_eq!(
        sessions.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        sessions.body
    );
}

#[tokio::test]
async fn get_workers_other_member_is_403() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let workers = call(
        &app,
        "GET",
        &fleet(&a_ws, "/workers"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(workers.status, StatusCode::FORBIDDEN, "{}", workers.body);
}

#[tokio::test]
async fn get_session_detail_other_workspace_path_is_404() {
    let (app, state) = app_with_state().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must be distinct workspaces");
    let sid = plant_remote_session(&app, &state, &a_jar, &a_ws).await;

    let detail = call(
        &app,
        "GET",
        &fleet(&b_ws, &format!("/sessions/{sid}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(detail.status, StatusCode::NOT_FOUND, "{}", detail.body);
    assert!(
        detail.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "cross-workspace session detail must publish ErrorBody.error: {}",
        detail.body
    );
}

// ── 7. personas ─────────────────────────────────────────────────────────────

fn persona_list_ids(body: &Value) -> Vec<&str> {
    body["personas"]
        .as_array()
        .unwrap_or_else(|| panic!("personas array: {body}"))
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect()
}

async fn create_custom_persona(app: &axum::Router, jar: &CookieJar, base: &str) -> String {
    let created = call(
        app,
        "POST",
        base,
        Some(jar),
        Some(json!({
            "name": "Custom reviewer",
            "slug": format!("custom-reviewer-{}", Uuid::new_v4().as_simple()),
            "description": "Reviews PRs",
            "role_prompt": "Be thorough.",
            "worker_profile": "coding"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("persona id: {}", created.body))
        .to_string()
}

async fn archive_persona(app: &axum::Router, jar: &CookieJar, base: &str, pid: &str) {
    let archived = call(app, "DELETE", &format!("{base}/{pid}"), Some(jar), None).await;
    assert_eq!(archived.status, StatusCode::NO_CONTENT, "{}", archived.body);
}

#[tokio::test]
async fn seed_starters_first_inserts_eight_skipped_zero() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");

    let first = call(
        &app,
        "POST",
        &format!("{base}/seed-starters"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["inserted"], 8, "{}", first.body);
    assert_eq!(first.body["skipped"], 0, "{}", first.body);
}

#[tokio::test]
async fn patch_persona_name_persists() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");
    let pid = create_custom_persona(&app, &jar, &base).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({ "name": "Reviewer v2" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["name"], "Reviewer v2", "{}", patched.body);
}

#[tokio::test]
async fn default_persona_list_omits_archived_after_delete() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");
    let pid = create_custom_persona(&app, &jar, &base).await;
    archive_persona(&app, &jar, &base, &pid).await;

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = persona_list_ids(&listed.body);
    assert!(
        !ids.contains(&pid.as_str()),
        "default persona list is active-only; archived {pid} leaked: {ids:?}"
    );
}

#[tokio::test]
async fn list_personas_status_archived_includes_soft_deleted() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");
    let pid = create_custom_persona(&app, &jar, &base).await;
    archive_persona(&app, &jar, &base, &pid).await;

    let listed = call(
        &app,
        "GET",
        &format!("{base}?status=archived"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = persona_list_ids(&listed.body);
    assert!(
        ids.contains(&pid.as_str()),
        "GET ?status=archived must include soft-deleted {pid}: {ids:?}"
    );
}

#[tokio::test]
async fn delete_persona_already_archived_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");
    let pid = create_custom_persona(&app, &jar, &base).await;
    archive_persona(&app, &jar, &base, &pid).await;

    let second = call(&app, "DELETE", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(second.status, StatusCode::NOT_FOUND, "{}", second.body);
}

#[tokio::test]
async fn patch_persona_status_active_unarchives() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let base = format!("/api/workspaces/{ws}/personas");
    let pid = create_custom_persona(&app, &jar, &base).await;
    archive_persona(&app, &jar, &base, &pid).await;

    let unarchived = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({ "status": "active" })),
    )
    .await;
    assert_eq!(unarchived.status, StatusCode::OK, "{}", unarchived.body);
    assert_eq!(unarchived.body["status"], "active", "{}", unarchived.body);

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = persona_list_ids(&listed.body);
    assert!(
        ids.contains(&pid.as_str()),
        "unarchived {pid} must appear on the default active list: {ids:?}"
    );
}

// ── 8. integrations + oauth ─────────────────────────────────────────────────
// Unconfigured bootstrap HTML, Trello #token, unknown start 404, no-cookie
// callback, and typ=access state stay in catalog.rs / routes.rs units and
// oauth_callback_requires_session_cookie.

#[tokio::test]
async fn integrations_list_oauth_start_path_available_not_coming_soon() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let items = list.body["integrations"]
        .as_array()
        .unwrap_or_else(|| panic!("integrations array: {}", list.body));
    let github = items
        .iter()
        .find(|i| i["id"] == "github")
        .unwrap_or_else(|| panic!("github missing: {}", list.body));
    let bitbucket = items
        .iter()
        .find(|i| i["id"] == "bitbucket")
        .unwrap_or_else(|| panic!("bitbucket missing: {}", list.body));
    assert_eq!(
        github["oauth_start_path"],
        format!("/api/workspaces/{ws}/integrations/github/oauth/start"),
        "{github}"
    );
    assert_eq!(
        bitbucket["oauth_start_path"],
        format!("/api/workspaces/{ws}/integrations/bitbucket/oauth/start"),
        "{bitbucket}"
    );
}

#[tokio::test]
async fn integrations_unknown_oauth_start_is_not_found() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/not-a-provider/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert!(
        start.status == StatusCode::NOT_FOUND || start.status == StatusCode::BAD_REQUEST,
        "{}",
        start.body
    );
}

#[tokio::test]
async fn integrations_bitbucket_oauth_start_is_connect_form() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/bitbucket/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(start.status, StatusCode::OK, "{}", start.body);
    assert!(
        start.raw.to_ascii_lowercase().contains("bitbucket"),
        "{}",
        start.raw
    );
}

#[tokio::test]
async fn integrations_disconnect_not_connected_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let disc = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/integrations/github"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(disc.status, StatusCode::NOT_FOUND, "{}", disc.body);
}

#[tokio::test]
async fn integrations_disconnect_connected_returns_ok_and_list_not_connected() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    seed_inbox_connection(&state, workspace_id, "github").await;

    let disc = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/integrations/github"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(disc.status, StatusCode::OK, "{}", disc.body);
    assert_eq!(disc.body["disconnected"], true, "{}", disc.body);

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let items = list.body["integrations"]
        .as_array()
        .unwrap_or_else(|| panic!("integrations array: {}", list.body));
    let github = items
        .iter()
        .find(|i| i["id"] == "github")
        .unwrap_or_else(|| panic!("github missing: {}", list.body));
    assert_eq!(github["connected"], false, "{github}");

    let creds = fleet_db::repos::integrations::credentials_for(&state.db, workspace_id, "github")
        .await
        .expect("credentials_for");
    assert!(
        creds.is_none(),
        "credentials_for must be empty after HTTP disconnect, got {creds:?}"
    );
}

#[tokio::test]
async fn integrations_other_workspace_cannot_list_start_or_delete() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{a_ws}/integrations"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN, "{}", list.body);

    let start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{a_ws}/integrations/github/oauth/start"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(start.status, StatusCode::FORBIDDEN, "{}", start.body);

    let disc = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{a_ws}/integrations/github"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(disc.status, StatusCode::FORBIDDEN, "{}", disc.body);
}

#[tokio::test]
async fn integrations_list_without_session_is_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        None,
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::UNAUTHORIZED, "{}", list.body);
}

/// PATCH defaults: unknown and not-connected known providers are 404 and
/// must not invent an integration_connections row.
#[tokio::test]
async fn patch_defaults_http_unknown_or_not_connected_is_404_and_does_not_insert() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");

    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM integration_connections WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await
    .expect("count integration_connections");

    let body = json!({
        "default_runner_id": "runner-1",
        "default_provider": "grok",
        "default_model": "model-1",
    });

    let unknown = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/integrations/notaprovider"),
        Some(&jar),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        unknown.status,
        StatusCode::NOT_FOUND,
        "unknown provider PATCH: {}",
        unknown.body
    );

    let missing = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/integrations/github"),
        Some(&jar),
        Some(body),
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::NOT_FOUND,
        "not-connected github PATCH: {}",
        missing.body
    );

    let after = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM integration_connections WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await
    .expect("count integration_connections after");
    assert_eq!(
        after, count,
        "PATCH defaults must not INSERT integration_connections"
    );
}

// ── 9. chat conversations ───────────────────────────────────────────────────

fn chat_conversations_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations")
}

fn chat_conversation_path(ws: &str, cid: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations/{cid}")
}

fn conversation_ids(body: &Value) -> Vec<&str> {
    body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations array: {body}"))
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect()
}

async fn invite_existing_member(app: &axum::Router, owner_jar: &CookieJar, ws: &str) -> CookieJar {
    let (member_email, member_jar, _member_ws) = signup_and_login(app).await;
    let invited = call(
        app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        invited.status,
        StatusCode::CREATED,
        "invite teammate: {}",
        invited.body
    );
    assert_eq!(
        invited.body["added_existing_user"],
        false,
        "invite of an existing user is pending until accept: {}",
        invited.body
    );
    let token = invited.body["invite"]["token"]
        .as_str()
        .expect("invite.token");
    let accepted = call(
        app,
        "POST",
        "/api/invites/accept",
        Some(&member_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(
        accepted.status,
        StatusCode::OK,
        "accept teammate invite: {}",
        accepted.body
    );
    member_jar
}

async fn create_chat_conversation(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    title: &str,
) -> String {
    let created = call(
        app,
        "POST",
        &chat_conversations_path(ws),
        Some(jar),
        Some(json!({ "title": title })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    created.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created.body))
        .to_string()
}

/// GET /chat/status with no runner_id is control-plane OpenAI + Gemini only.
#[tokio::test]
async fn chat_status_without_runner_id_is_openai_only() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let status = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/chat/status"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    let providers = status.body["providers"].as_array().expect("providers");
    let ids: Vec<&str> = providers
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        ["openai", "gemini"],
        "without runner_id, inventory is plane OpenAI+Gemini (not runner Grok/Claude/Ollama): {}",
        status.body
    );
    let hint = status.body["hint"].as_str().unwrap_or("");
    assert!(
        hint.to_ascii_lowercase().contains("runner"),
        "status must point at the runner inventory: {hint}"
    );
}

/// Invited member GET/DELETE of the owner's thread is 404; the row stays.
#[tokio::test]
async fn chat_conversation_get_delete_as_teammate_is_404() {
    let app = app().await;
    let (_email, owner_jar, ws) = signup_and_login(&app).await;
    let cid = create_chat_conversation(&app, &owner_jar, &ws, "owner thread").await;
    let member_jar = invite_existing_member(&app, &owner_jar, &ws).await;
    let path = chat_conversation_path(&ws, &cid);

    let got = call(&app, "GET", &path, Some(&member_jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::NOT_FOUND,
        "teammate GET must be 404 conversation, not 403 membership: {} {}",
        got.status,
        got.body
    );

    let deleted = call(&app, "DELETE", &path, Some(&member_jar), None).await;
    assert_eq!(
        deleted.status,
        StatusCode::NOT_FOUND,
        "teammate DELETE must be 404 conversation, not 403 membership: {} {}",
        deleted.status,
        deleted.body
    );
    assert_ne!(
        deleted.body.get("deleted"),
        Some(&json!(true)),
        "teammate DELETE must not report deleted=true: {}",
        deleted.body
    );

    let leftover = call(&app, "GET", &path, Some(&owner_jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    assert_eq!(
        leftover.body["conversation"]["id"].as_str(),
        Some(cid.as_str()),
        "owner row must stay after teammate DELETE: {}",
        leftover.body
    );
}

/// Two members each own a thread; list is user_id-scoped, not workspace-wide.
#[tokio::test]
async fn chat_conversations_list_is_user_scoped() {
    let (app, state) = app_with_state().await;
    let (_owner_email, owner_jar, ws) = signup_and_login(&app).await;
    let member_jar = invite_existing_member(&app, &owner_jar, &ws).await;
    let owner_cid = create_chat_conversation(
        &app,
        &owner_jar,
        &ws,
        &format!("owner-{}", Uuid::new_v4()),
    )
    .await;
    let member_cid = create_chat_conversation(
        &app,
        &member_jar,
        &ws,
        &format!("member-{}", Uuid::new_v4()),
    )
    .await;
    // Empty threads stay off the list. A saved line is what makes the row visible.
    for cid in [&owner_cid, &member_cid] {
        fleet_db::repos::chats::insert_message(
            &state.db,
            Uuid::now_v7(),
            Uuid::parse_str(cid).expect("cid"),
            "user",
            "hello",
            None,
            None,
            &json!({}),
            chrono::Utc::now(),
        )
        .await
        .expect("insert message");
    }
    let list_path = chat_conversations_path(&ws);

    let owner_list = call(&app, "GET", &list_path, Some(&owner_jar), None).await;
    assert_eq!(owner_list.status, StatusCode::OK, "{}", owner_list.body);
    let owner_ids = conversation_ids(&owner_list.body);
    assert_eq!(
        owner_ids,
        vec![owner_cid.as_str()],
        "owner list must include only the owner's id: {}",
        owner_list.body
    );

    let member_list = call(&app, "GET", &list_path, Some(&member_jar), None).await;
    assert_eq!(member_list.status, StatusCode::OK, "{}", member_list.body);
    let member_ids = conversation_ids(&member_list.body);
    assert_eq!(
        member_ids,
        vec![member_cid.as_str()],
        "member list must include only the member's id: {}",
        member_list.body
    );
}

/// Foreign workspace cookie is 403 membership on conversation CRUD.
#[tokio::test]
async fn chat_conversations_other_workspace_cookie_is_403() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let cid = create_chat_conversation(&app, &a_jar, &a_ws, "owner thread").await;
    let list_path = chat_conversations_path(&a_ws);
    let item_path = chat_conversation_path(&a_ws, &cid);

    let listed = call(&app, "GET", &list_path, Some(&b_jar), None).await;
    assert_eq!(listed.status, StatusCode::FORBIDDEN, "{}", listed.body);

    let created = call(
        &app,
        "POST",
        &list_path,
        Some(&b_jar),
        Some(json!({ "title": format!("other-ws-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(created.status, StatusCode::FORBIDDEN, "{}", created.body);

    let got = call(&app, "GET", &item_path, Some(&b_jar), None).await;
    assert_eq!(got.status, StatusCode::FORBIDDEN, "{}", got.body);

    let deleted = call(&app, "DELETE", &item_path, Some(&b_jar), None).await;
    assert_eq!(deleted.status, StatusCode::FORBIDDEN, "{}", deleted.body);
}

// ── 10. chat tools without live LLM (direct execute_tool + host inbox) ──────

fn chat_tool_llm() -> fleet_server::chat_tools::ChatLlmContext {
    fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    }
}

async fn authed_user_and_workspace(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
) -> (Uuid, Uuid) {
    let me = call(app, "GET", "/api/auth/me", Some(jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(ws).expect("workspace uuid");
    (user_id, workspace_id)
}

async fn patch_min_isolation(app: &axum::Router, jar: &CookieJar, ws: &str, isolation: &str) {
    let patched = call(
        app,
        "PATCH",
        &fleet(ws, "/settings"),
        Some(jar),
        Some(json!({ "min_isolation": isolation })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH min_isolation={isolation}: {}",
        patched.body
    );
    assert_eq!(
        patched.body["min_isolation"], isolation,
        "PATCH min_isolation={isolation}: {}",
        patched.body
    );
}

fn ticks_spawned(content: &Value) -> u64 {
    content["ticks"]
        .as_array()
        .map(|ticks| {
            ticks
                .iter()
                .map(|t| t["spawned"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

fn first_task_status(content: &Value) -> &Value {
    let tasks = content["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks array: {content}"));
    assert_eq!(tasks.len(), 1, "launch_agent_task is a 1-task plan: {content}");
    &tasks[0]["status"]
}

fn bff_run<'a>(runs: &'a Value, run_id: &str) -> &'a Value {
    runs["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("runs array: {runs}"))
        .iter()
        .find(|r| r["id"].as_str() == Some(run_id))
        .unwrap_or_else(|| panic!("BFF runs missing {run_id}: {runs}"))
}

fn bff_task_for_run<'a>(board: &'a Value, run_id: &str) -> &'a Value {
    board["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks array: {board}"))
        .iter()
        .find(|t| t["run_id"].as_str() == Some(run_id))
        .unwrap_or_else(|| panic!("BFF tasks missing run {run_id}: {board}"))
}

async fn complete_one_host_lease(app: &axum::Router, machine: &str, ws: &str) {
    let lease = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/lease",
        machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    let claim = &lease.body["claim"];
    assert!(
        claim.is_object(),
        "workspace host must lease the waiting chat task: {}",
        lease.body
    );
    let id = claim["id"]
        .as_str()
        .unwrap_or_else(|| panic!("claim.id: {}", lease.body));
    let tok = claim["lease_token"]
        .as_str()
        .unwrap_or_else(|| panic!("claim.lease_token: {}", lease.body));
    let session_id = claim["payload"]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("payload.session_id: {}", lease.body));
    let worker_token = claim["payload"]["worker_token"]
        .as_str()
        .unwrap_or_else(|| panic!("payload.worker_token: {}", lease.body));
    assert!(
        !id.is_empty() && !tok.is_empty() && !session_id.is_empty() && !worker_token.is_empty(),
        "lease claim must carry id, lease_token, session_id, worker_token: {}",
        lease.body
    );

    let status = call_bearer(
        app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        worker_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "result_md": "Task completed successfully with artifacts.",
            "files": { "work/out.txt": "ok" }
        })),
    )
    .await;
    assert_eq!(
        status.status,
        StatusCode::ACCEPTED,
        "worker status: {}",
        status.body
    );

    let done = call_bearer(
        app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{id}/complete"),
        machine,
        Some(json!({ "lease_token": tok, "ok": true })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "lease complete: {}",
        done.body
    );
}

/// Empty assignable pool (min_isolation=vm, no VM host) leaves the chat run
/// open: run_terminal=false, the card ready, BFF run id present.
#[tokio::test]
async fn chat_tool_launch_agent_task_waits_for_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    // Public pin. Keep vm so a shared cluster this-machine host cannot assign.
    patch_min_isolation(&app, &jar, &ws, "vm").await;

    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "http_api host launch",
            "spec": "Leave ready until a paired host reports via inbox.",
            "worker_profile": "coding"
        }),
        &chat_tool_llm(),
    )
    .await;

    assert!(
        action.ok,
        "launch_agent_task should accept the plan: action={action:?} content={content}"
    );
    assert_eq!(action.tool, "launch_agent_task");
    let run_id = action
        .run_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("run_id: {action:?}"));
    assert_eq!(
        content["run_terminal"], false,
        "empty pool must leave the run open: {content}"
    );
    let run_status = content["run_status"].as_str().unwrap_or("");
    assert!(
        !matches!(run_status, "completed" | "failed" | "cancelled"),
        "run must stay non-terminal: {content}"
    );
    assert_eq!(content["task_count"], 1, "{content}");
    assert_eq!(
        first_task_status(&content),
        "ready",
        "chat task stays ready with no assignable host: {content}"
    );
    assert_eq!(
        ticks_spawned(&content),
        0,
        "empty pool must not spawn: {content}"
    );

    let board = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(board.status, StatusCode::OK, "{}", board.body);
    assert_eq!(
        bff_task_for_run(&board.body, run_id)["status"],
        "ready",
        "BFF task for {run_id}: {}",
        board.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    let run = bff_run(&runs.body, run_id);
    assert!(
        !matches!(
            run["status"].as_str().unwrap_or(""),
            "completed" | "failed" | "cancelled"
        ),
        "BFF run {run_id} must stay open: {}",
        runs.body
    );
}

/// After a waiting chat launch, pair a workspace host that meets the pin,
/// then lease/status/complete must finish that same BFF card.
#[tokio::test]
async fn chat_tool_launch_agent_task_delayed_host_inbox() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    patch_min_isolation(&app, &jar, &ws, "vm").await;

    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "http_api delayed host",
            "spec": "Leave ready until a paired host reports via inbox.",
            "worker_profile": "coding"
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "launch: action={action:?} content={content}");
    assert_eq!(
        content["run_terminal"], false,
        "setup must leave the run open: {content}"
    );
    let run_id = action
        .run_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("run_id: {action:?}"));

    // Pair before lowering the pin so the workspace host exists (home
    // prefer) when process hosts become assignable.
    let machine = pair_test_host(&app, &jar, &ws).await;
    patch_min_isolation(&app, &jar, &ws, "process").await;

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "assign tick: {}", tick.body);
    complete_one_host_lease(&app, &machine, &ws).await;
    let apply = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(apply.status, StatusCode::OK, "inbox tick: {}", apply.body);

    let board = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(board.status, StatusCode::OK, "{}", board.body);
    assert_eq!(
        bff_task_for_run(&board.body, run_id)["status"],
        "done_success",
        "host inbox should finish run {run_id}: {}",
        board.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    assert_eq!(
        bff_run(&runs.body, run_id)["status"],
        "completed",
        "BFF run {run_id} after inbox: {}",
        runs.body
    );
}

/// Pair+heartbeat first, then launch_agent_task: empty-pool wait arm is not taken.
#[tokio::test]
async fn launch_agent_task_with_host_already_paired_does_not_claim_waiting() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    // Column default is container; this fixture pairs a process-only PC.
    patch_min_isolation(&app, &jar, &ws, "process").await;
    let _machine = pair_test_host(&app, &jar, &ws).await;

    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "http_api already paired",
            "spec": "Assign onto the host that is already paired.",
            "worker_profile": "coding"
        }),
        &chat_tool_llm(),
    )
    .await;

    assert!(action.ok, "launch: action={action:?} content={content}");
    assert_eq!(action.tool, "launch_agent_task");
    let run_id = action
        .run_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("run_id: {action:?}"));
    assert!(
        ticks_spawned(&content) > 0,
        "already-paired host must be selected (not the empty-pool wait arm): {content}"
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    let _ = bff_run(&runs.body, run_id);
}

/// min_isolation=vm stays on; a process-only advertised_host does not pick up the card.
#[tokio::test]
async fn launch_agent_task_vm_need_stays_ready_when_only_process_host_pairs() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    patch_min_isolation(&app, &jar, &ws, "vm").await;

    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "http_api vm pin",
            "spec": "Stay ready; process-only host must not meet vm.",
            "worker_profile": "coding"
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "launch: action={action:?} content={content}");
    let run_id = action
        .run_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("run_id: {action:?}"));

    let _machine = pair_test_host(&app, &jar, &ws).await;
    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let board = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(board.status, StatusCode::OK, "{}", board.body);
    assert_eq!(
        bff_task_for_run(&board.body, run_id)["status"],
        "ready",
        "process-only host must not take a vm-need chat task: {}",
        board.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    let run = bff_run(&runs.body, run_id);
    assert!(
        !matches!(
            run["status"].as_str().unwrap_or(""),
            "completed" | "failed" | "cancelled"
        ),
        "vm pin must leave run {run_id} open: {}",
        runs.body
    );
}

// ── 11. /v1 admin bearer vs cookie ──────────────────────────────────────────

#[tokio::test]
async fn v1_projects_rejects_missing_bearer_and_session_cookie() {
    let app = app().await;
    let (_email, jar, _ws) = signup_and_login(&app).await;
    let body = json!({ "name": format!("v1-{}", Uuid::new_v4()), "max_agents": 2 });

    let cookie_only = call(&app, "POST", "/v1/projects", Some(&jar), Some(body.clone())).await;
    assert_eq!(
        cookie_only.status,
        StatusCode::UNAUTHORIZED,
        "cookie must not authorize /v1: {}",
        cookie_only.body
    );
    assert_eq!(
        cookie_only.raw,
        "invalid admin token",
        "cookie 401 must be invalid admin token, not admin-automation-disabled: {}",
        cookie_only.raw
    );

    // Configured ADMIN_TOKEN bearer must still create so the 401 is not a dead /v1.
    let with_admin = call_bearer(&app, "POST", "/v1/projects", TEST_ADMIN_TOKEN, Some(body)).await;
    assert_eq!(
        with_admin.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN bearer should create a project: {}",
        with_admin.body
    );
}

#[tokio::test]
async fn v1_projects_rejects_wrong_admin_bearer() {
    let app = app().await;
    let body = json!({
        "name": format!("v1-wrong-{}", Uuid::new_v4()),
        "max_agents": 2
    });

    let wrong = call_bearer(&app, "POST", "/v1/projects", "not-the-token", Some(body)).await;
    assert_eq!(
        wrong.status,
        StatusCode::UNAUTHORIZED,
        "Bearer not-the-token: {}",
        wrong.body
    );
    assert_eq!(
        wrong.raw,
        "invalid admin token",
        "wrong Bearer must be invalid admin token, not admin-automation-disabled: {}",
        wrong.raw
    );
}

#[tokio::test]
async fn v1_admin_mutating_routes_reject_session_cookie() {
    let app = app().await;
    let (_email, jar, _ws) = signup_and_login(&app).await;
    let created = call_bearer(
        &app,
        "POST",
        "/v1/projects",
        TEST_ADMIN_TOKEN,
        Some(json!({ "name": format!("v1-mut-{}", Uuid::new_v4()), "max_agents": 2 })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("project id: {}", created.body));

    let tick = call(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        tick.status,
        StatusCode::UNAUTHORIZED,
        "cookie POST admin/tick: {}",
        tick.body
    );
    assert_eq!(tick.raw, "invalid admin token", "{}", tick.raw);

    let fixture = call(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        Some(&jar),
        Some(json!({
            "plan": {
                "plan_id": Uuid::new_v4(),
                "project_id": pid,
                "run_id": Uuid::new_v4(),
                "version": 1,
                "status": "accepted",
                "source": "fixture",
                "tasks": [],
                "policy": { "on_task_fail": "fail_run" }
            }
        })),
    )
    .await;
    assert_eq!(
        fixture.status,
        StatusCode::UNAUTHORIZED,
        "cookie POST runs/fixture: {}",
        fixture.body
    );
    assert_eq!(fixture.raw, "invalid admin token", "{}", fixture.raw);

    let patched = call(
        &app,
        "PATCH",
        &format!("/v1/projects/{pid}/config"),
        Some(&jar),
        Some(json!({ "repo_url": "https://example.test/r.git" })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::UNAUTHORIZED,
        "cookie PATCH config: {}",
        patched.body
    );
    assert_eq!(patched.raw, "invalid admin token", "{}", patched.raw);
}

#[tokio::test]
async fn v1_get_project_rejects_session_cookie() {
    let app = app().await;
    let (_email, jar, _ws) = signup_and_login(&app).await;
    let created = call_bearer(
        &app,
        "POST",
        "/v1/projects",
        TEST_ADMIN_TOKEN,
        Some(json!({ "name": format!("v1-get-{}", Uuid::new_v4()), "max_agents": 2 })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("project id: {}", created.body));

    let cookie_get = call(
        &app,
        "GET",
        &format!("/v1/projects/{pid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        cookie_get.status,
        StatusCode::UNAUTHORIZED,
        "cookie GET /v1/projects/{{id}}: {}",
        cookie_get.body
    );
    assert_eq!(
        cookie_get.raw,
        "invalid admin token",
        "cookie GET must be invalid admin token: {}",
        cookie_get.raw
    );
}

#[tokio::test]
async fn fleet_requires_membership() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let anon = call(&app, "GET", &fleet(&a_ws, ""), None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{}", anon.body);

    let other = call(&app, "GET", &fleet(&a_ws, ""), Some(&b_jar), None).await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.body);
}

#[tokio::test]
async fn fleet_other_workspace_cannot_post_tick_or_patch_settings() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let tick = call(&app, "POST", &fleet(&a_ws, "/tick"), Some(&b_jar), None).await;
    assert_eq!(tick.status, StatusCode::FORBIDDEN, "{}", tick.body);

    let settings = call(
        &app,
        "PATCH",
        &fleet(&a_ws, "/settings"),
        Some(&b_jar),
        Some(json!({ "max_agents": 4 })),
    )
    .await;
    assert_eq!(settings.status, StatusCode::FORBIDDEN, "{}", settings.body);
}

// ── 12. fleet settings + agency-agents personas ─────────────────────────────

#[tokio::test]
async fn fleet_settings_get_defaults_fleet_llm_off() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let got = call(&app, "GET", &fleet(&ws, "/settings"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["fleet_llm"], "off", "{}", got.body);
    assert_eq!(got.body["fleet_llm_model"], "", "{}", got.body);
    assert_eq!(got.body["effective_backend"], "off", "{}", got.body);
    assert_eq!(got.body["max_agents"], 16, "{}", got.body);
}

#[tokio::test]
async fn fleet_settings_patch_ollama_persists_and_embeds_on_fleet() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let patched = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({
            "max_agents": 4,
            "fleet_llm": "ollama",
            "fleet_llm_model": "llama3.2"
        })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["max_agents"], 4, "{}", patched.body);
    assert_eq!(patched.body["fleet_llm"], "ollama", "{}", patched.body);
    assert_eq!(
        patched.body["fleet_llm_model"], "llama3.2",
        "{}",
        patched.body
    );
    assert_eq!(
        patched.body["effective_backend"], "ollama",
        "{}",
        patched.body
    );

    let project = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(project.status, StatusCode::OK, "{}", project.body);
    assert_eq!(
        project.body["config"]["fleet_llm"], "ollama",
        "GET /fleet must embed stored settings: {}",
        project.body
    );
    assert_eq!(
        project.body["config"]["fleet_llm_model"], "llama3.2",
        "{}",
        project.body
    );
    assert_eq!(project.body["config"]["max_agents"], 4, "{}", project.body);
}

#[tokio::test]
async fn fleet_settings_patch_grok_is_400_and_leaves_ollama() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({
            "max_agents": 4,
            "fleet_llm": "ollama",
            "fleet_llm_model": "llama3.2"
        })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let grok = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "fleet_llm": "grok" })),
    )
    .await;
    assert_eq!(grok.status, StatusCode::BAD_REQUEST, "{}", grok.body);
    assert!(
        grok.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "400 body must include error: {}",
        grok.body
    );

    let after = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["fleet_llm"], "ollama",
        "rejected grok PATCH must not coerce stored fleet_llm: {}",
        after.body
    );
    assert_eq!(
        after.body["fleet_llm_model"], "llama3.2",
        "{}",
        after.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_max_agents_zero_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 4 })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let out = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 0 })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert!(
        out.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "400 body must include error: {}",
        out.body
    );

    let after = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["max_agents"], 4,
        "rejected max_agents=0 must not clamp-store: {}",
        after.body
    );
}

#[tokio::test]
async fn fleet_settings_patch_max_agents_above_cap_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 4 })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let out = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({
            "max_agents": fleet_orchestrator::domain::MAX_PROJECT_AGENTS + 1
        })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert!(
        out.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "400 body must include error: {}",
        out.body
    );

    let after = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["max_agents"], 4,
        "rejected max_agents above cap must not clamp-store: {}",
        after.body
    );
}

#[tokio::test]
async fn fleet_settings_get_require_membership() {
    let app = app().await;
    let (_a, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let anon = call(&app, "GET", &fleet(&a_ws, "/settings"), None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{}", anon.body);

    let other = call(
        &app,
        "GET",
        &fleet(&a_ws, "/settings"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.body);
}

#[tokio::test]
async fn fleet_settings_patch_off_after_ollama() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let settings = fleet(&ws, "/settings");

    let setup = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({
            "fleet_llm": "ollama",
            "fleet_llm_model": "llama3.2"
        })),
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    let off = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "fleet_llm": "off" })),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.body);
    assert_eq!(off.body["fleet_llm"], "off", "{}", off.body);
    assert_eq!(off.body["fleet_llm_model"], "", "{}", off.body);
    assert_eq!(off.body["effective_backend"], "off", "{}", off.body);
}

#[tokio::test]
async fn fleet_personas_lists_builtin_or_roster() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let out = call(&app, "GET", &fleet(&ws, "/personas"), Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(
        out.body["source"],
        "https://github.com/msitarzewski/agency-agents",
        "{}",
        out.body
    );
    let personas = out.body["personas"]
        .as_array()
        .unwrap_or_else(|| panic!("personas array: {}", out.body));
    assert_eq!(
        out.body["count"].as_u64().unwrap_or(0) as usize,
        personas.len(),
        "count must match personas array: {}",
        out.body
    );
    let slugs: Vec<&str> = personas
        .iter()
        .filter_map(|p| p["slug"].as_str())
        .collect();
    // Builtins and the cloned agency-agents roster both ship these three
    // slugs; `generalist` is fallback-only (no generalist.md upstream).
    for slug in [
        "engineering-backend-architect",
        "engineering-code-reviewer",
        "engineering-frontend-developer",
    ] {
        assert!(
            slugs.contains(&slug),
            "missing roster slug {slug} in {slugs:?}: {}",
            out.body
        );
    }
}

#[tokio::test]
async fn chat_tool_launch_agent_plan_stamps_llm() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5-internal".into(),
        runner_id: None,
        effort: None,
    };

    let (content, launched) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "impl", "title": "Implement", "spec": "do it", "worker_profile": "coding" }
            ]
        }),
        &llm,
    )
    .await;
    assert!(launched.ok, "{launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");
    let run_uuid = Uuid::parse_str(run_id).unwrap();
    let snap = state
        .orch
        .store
        .load_board_snapshot(workspace_id)
        .await
        .unwrap();
    let plan_id = snap
        .tasks
        .iter()
        .find(|t| t.run_id == run_uuid)
        .unwrap()
        .plan_id;
    let body = state
        .orch
        .store
        .get_plan_body(plan_id)
        .await
        .unwrap()
        .expect("plan");
    assert_eq!(body.policy.llm_provider.as_deref(), Some("grok"));
    assert_eq!(body.policy.llm_model.as_deref(), Some("grok-4.5-internal"));
}

#[tokio::test]
async fn list_agent_personas_returns_default_generalist_and_aliases() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_agent_personas",
        &json!({}),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "{action:?} {listed}");
    assert_eq!(action.tool, "list_agent_personas");
    assert_eq!(listed["default"], "generalist", "{listed}");
    let aliases = listed["aliases"]
        .as_object()
        .unwrap_or_else(|| panic!("aliases object: {listed}"));
    for key in ["coding", "frontend", "reviewer", "testing"] {
        assert!(
            aliases.contains_key(key),
            "missing alias {key} in {aliases:?}: {listed}"
        );
    }
}

#[tokio::test]
async fn launch_agent_plan_persists_coding_and_reviewer_depends_on() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (content, launched) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "impl", "title": "Implement", "spec": "do it", "worker_profile": "coding" },
                { "id": "rev", "title": "Review", "spec": "review it", "worker_profile": "reviewer", "depends_on": ["impl"] }
            ]
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(launched.ok, "{launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");
    let run_uuid = Uuid::parse_str(run_id).unwrap();
    let snap = state
        .orch
        .store
        .load_board_snapshot(workspace_id)
        .await
        .unwrap();
    let impl_task = snap
        .tasks
        .iter()
        .find(|t| t.run_id == run_uuid && t.plan_task_id == "impl")
        .unwrap_or_else(|| panic!("impl task missing for {run_uuid}"));
    let rev = snap
        .tasks
        .iter()
        .find(|t| t.run_id == run_uuid && t.plan_task_id == "rev")
        .unwrap_or_else(|| panic!("rev task missing for {run_uuid}"));
    assert_eq!(impl_task.worker_profile, "coding");
    assert_eq!(rev.worker_profile, "reviewer");
    assert!(
        rev.depends_on.contains(&impl_task.id),
        "rev.depends_on must contain impl {}: {:?}",
        impl_task.id,
        rev.depends_on
    );
}

#[tokio::test]
async fn list_agent_personas_includes_workspace_custom_slug() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    let slug = format!("ws-custom-{}", Uuid::new_v4().as_simple());
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/personas"),
        Some(&jar),
        Some(json!({
            "name": "Workspace custom",
            "slug": slug,
            "description": "Custom workspace persona",
            "role_prompt": "Be custom.",
            "worker_profile": "coding"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["slug"], slug, "{}", created.body);

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_agent_personas",
        &json!({}),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "{action:?} {listed}");
    let workspace_personas = listed["workspace_personas"]
        .as_array()
        .unwrap_or_else(|| panic!("workspace_personas array: {listed}"));
    assert!(
        workspace_personas.iter().any(|p| p["slug"] == slug),
        "workspace_personas missing {slug}: {listed}"
    );
}

#[tokio::test]
async fn chat_tools_human_can_talk_on_team_channel() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (content, launched) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "t1", "title": "API", "spec": "build it", "worker_profile": "coding" }
            ]
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(launched.ok, "setup launch: {launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");

    let (sent, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": run_id, "to": "t1", "body": "use port 9090" }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "{action:?} {sent}");
    assert_eq!(sent["from"], "human", "{sent}");
    assert_eq!(sent["to"], "t1", "{sent}");
    assert_eq!(sent["body"], "use port 9090", "{sent}");

    let (read, read_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "read_team_chat",
        &json!({ "run_id": run_id }),
        &chat_tool_llm(),
    )
    .await;
    assert!(read_action.ok, "{read_action:?} {read}");
    assert_eq!(read["count"], 1, "{read}");
    assert_eq!(read["messages"][0]["from"], "human", "{read}");
    assert_eq!(read["messages"][0]["to"], "t1", "{read}");
    assert_eq!(read["messages"][0]["body"], "use port 9090", "{read}");
}

#[tokio::test]
async fn send_team_message_empty_or_whitespace_body_is_not_ok() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (content, launched) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "t1", "title": "API", "spec": "build it", "worker_profile": "coding" }
            ]
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(launched.ok, "setup launch: {launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");

    for body in ["", "   "] {
        let (sent, action) = fleet_server::chat_tools::execute_tool(
            &state,
            workspace_id,
            user_id,
            "send_team_message",
            &json!({ "run_id": run_id, "to": "t1", "body": body }),
            &chat_tool_llm(),
        )
        .await;
        assert!(
            !action.ok,
            "blank body {body:?} must not succeed: {action:?} {sent}"
        );
    }
}

#[tokio::test]
async fn read_team_chat_with_no_runs_is_not_ok() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (read, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "read_team_chat",
        &json!({}),
        &chat_tool_llm(),
    )
    .await;
    assert!(
        !action.ok,
        "read_team_chat with no fleet runs must not succeed: {action:?} {read}"
    );
}

#[tokio::test]
async fn overview_chat_echoes_human_from_to_and_body() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;

    let (content, launched) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "t1", "title": "API", "spec": "build it", "worker_profile": "coding" }
            ]
        }),
        &chat_tool_llm(),
    )
    .await;
    assert!(launched.ok, "setup launch: {launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");

    let (sent, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": run_id, "to": "t1", "body": "use port 9090" }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "setup send: {action:?} {sent}");

    let overview = call(&app, "GET", &fleet(&ws, "/overview"), Some(&jar), None).await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    let chat = overview.body["chat"]
        .as_array()
        .unwrap_or_else(|| panic!("chat array: {}", overview.body));
    assert_eq!(chat.len(), 1, "{}", overview.body);
    assert_eq!(chat[0]["from"], "human", "{}", overview.body);
    assert_eq!(chat[0]["to"], "t1", "{}", overview.body);
    assert_eq!(chat[0]["body"], "use port 9090", "{}", overview.body);
}

async fn seed_inbox_ticket(
    state: &AppState,
    workspace_id: Uuid,
    key: &str,
    title: &str,
    closed: bool,
) {
    fleet_db::repos::tickets::upsert_inbox(
        &state.db,
        &fleet_db::repos::tickets::UpsertInbox {
            workspace_id,
            provider: "github".into(),
            external_key: key.into(),
            title: title.into(),
            url: None,
            summary: title.into(),
            provider_closed: closed,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("seed inbox {key}: {e}"));
}

#[tokio::test]
async fn chat_tools_ticket_inbox_is_available() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    let key = format!("list-tickets-{}/app#1", Uuid::new_v4());
    seed_inbox_ticket(&state, workspace_id, &key, "seeded inbox row", false).await;

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_tickets",
        &json!({}),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "list_tickets {action:?} {listed}");
    assert_eq!(action.tool, "list_tickets", "{action:?}");
    assert_eq!(listed["count"], 1, "{listed}");
    let tickets = listed["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets array: {listed}"));
    assert_eq!(tickets.len(), 1, "{listed}");
    assert_eq!(tickets[0]["key"], key, "{listed}");
}

#[tokio::test]
async fn chat_tools_list_tickets_state_filter_returns_only_matching() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    let tag = Uuid::new_v4();
    let open_key = format!("state-tool-{tag}/app#open");
    let launched_key = format!("state-tool-{tag}/app#launched");
    seed_inbox_ticket(&state, workspace_id, &open_key, "open ticket", false).await;
    seed_inbox_ticket(&state, workspace_id, &launched_key, "launched ticket", false).await;
    fleet_db::repos::tickets::mark_launched(
        &state.db,
        workspace_id,
        "github",
        &launched_key,
        Uuid::now_v7(),
    )
    .await
    .expect("mark launched")
    .expect("launched row");

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_tickets",
        &json!({ "state": "open" }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "list_tickets state=open {action:?} {listed}");
    assert_eq!(action.tool, "list_tickets", "{action:?}");
    assert_eq!(listed["count"], 1, "{listed}");
    let tickets = listed["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets array: {listed}"));
    assert_eq!(tickets.len(), 1, "{listed}");
    assert_eq!(tickets[0]["key"], open_key, "{listed}");
}

#[tokio::test]
async fn chat_tools_finish_ticket_loads_spec_without_launching() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    let key = format!("finish-ticket-{}/app#9", Uuid::new_v4());
    seed_inbox_ticket(&state, workspace_id, &key, "load spec only", false).await;

    let (loaded, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "finish_ticket",
        &json!({ "ticket": key }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "finish_ticket {action:?} {loaded}");
    assert_eq!(action.tool, "finish_ticket", "{action:?}");
    assert!(
        action.run_id.is_none(),
        "finish_ticket must not launch: {action:?}"
    );
    assert_eq!(loaded["launched"], false, "{loaded}");
    assert_eq!(loaded["title"], format!("{key}: load spec only"), "{loaded}");
    assert_eq!(loaded["ticket"]["key"], key, "{loaded}");
    let spec = loaded["spec"]
        .as_str()
        .unwrap_or_else(|| panic!("spec string: {loaded}"));
    assert!(
        spec.contains("load spec only"),
        "spec must include the seeded title: {loaded}"
    );
}

#[tokio::test]
async fn chat_tools_monitor_tickets_sets_enabled() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (user_id, workspace_id) = authed_user_and_workspace(&app, &jar, &ws).await;
    seed_inbox_connection(&state, workspace_id, "github").await;

    let (updated, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "monitor_tickets",
        &json!({ "enabled": true }),
        &chat_tool_llm(),
    )
    .await;
    assert!(action.ok, "monitor_tickets {action:?} {updated}");
    assert_eq!(action.tool, "monitor_tickets", "{action:?}");

    let inbox = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
    assert_eq!(
        inbox.body["monitor"]["enabled"], true,
        "monitor.enabled after monitor_tickets: {}",
        inbox.body
    );
}

#[tokio::test]
async fn list_integrations_connected_only_omits_disconnected() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    seed_inbox_connection(&state, workspace_id, "slack").await;
    seed_inbox_connection(&state, workspace_id, "jira").await;
    fleet_db::repos::integrations::disconnect(&state.db, workspace_id, "jira")
        .await
        .expect("disconnect jira");

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_integrations",
        &json!({ "connected_only": true }),
        &llm,
    )
    .await;
    assert!(action.ok, "{action:?} {listed}");
    let items = listed["integrations"]
        .as_array()
        .unwrap_or_else(|| panic!("integrations: {listed}"));
    let slack = items
        .iter()
        .find(|i| i["id"].as_str() == Some("slack"))
        .unwrap_or_else(|| panic!("missing slack id: {listed}"));
    assert_eq!(slack["connected"], true, "{listed}");
    assert!(
        items.iter().all(|i| i["id"].as_str() != Some("jira")),
        "connected_only must omit disconnected jira: {listed}"
    );
}

#[tokio::test]
async fn get_integration_planted_slack_connected_true_no_secrets() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    fleet_db::repos::integrations::upsert(
        &state.db,
        &fleet_db::repos::integrations::UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: "slack".into(),
            account_label: Some("planted-slack".into()),
            config: json!({}),
            credentials: json!({
                "_enc": "v1",
                "access_token": "planted-must-not-leak",
                "nonce": "aa00112233445566778899aa",
                "ct": "deadbeefcafebabe"
            }),
            connected_by: Some(user_id),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("plant slack");

    let (got, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "get_integration",
        &json!({ "provider": "slack" }),
        &llm,
    )
    .await;
    assert!(action.ok, "{action:?} {got}");
    assert_eq!(got["id"], "slack", "{got}");
    assert_eq!(got["connected"], true, "{got}");
    let blob = got.to_string();
    assert!(!blob.contains("credentials"), "{got}");
    assert!(!blob.contains("access_token"), "{got}");
    assert!(!blob.contains("_enc"), "{got}");
    assert!(!blob.contains("planted-must-not-leak"), "{got}");
}

#[tokio::test]
async fn get_integration_unknown_provider_not_ok() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    let (got, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "get_integration",
        &json!({ "provider": "not-a-provider" }),
        &llm,
    )
    .await;
    assert!(
        !action.ok,
        "unknown provider must not succeed: {action:?} {got}"
    );
}

#[tokio::test]
async fn cases_decide_use_tool_not_connected_is_tool_error() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "use-tool-slack").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "use_tool",
                "tool": {
                    "provider": "slack",
                    "operation": "get_channel_history",
                    "args": { "channel": "Cdoesnotexist" }
                }
            }
        })),
    )
    .await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    let result = &decided.body["context"]["last_tool_result"];
    assert_eq!(result["provider"], "slack", "{}", decided.body);
    assert_eq!(
        result["operation"], "get_channel_history",
        "{}",
        decided.body
    );
    assert_eq!(result["ok"], false, "{}", decided.body);
    assert_eq!(result["error_code"], "not_connected", "{}", decided.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    let kinds: Vec<&str> = got.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got.body))
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"tool_error"),
        "events must include kind tool_error: {kinds:?} body={}",
        got.body
    );
}

#[tokio::test]
async fn cases_decide_use_tool_missing_tool_is_tool_error() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "use-tool-missing").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "use_tool" }
        })),
    )
    .await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    assert_eq!(
        decided.body["context"]["last_tool_result"]["ok"],
        false,
        "{}",
        decided.body
    );
    let err = decided.body["last_error"].as_str().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("provider"),
        "last_error must mention provider: {err:?} body={}",
        decided.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    let kinds: Vec<&str> = got.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got.body))
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"tool_error"),
        "events must include kind tool_error: {kinds:?} body={}",
        got.body
    );
}

// ── 13. partners disclaimer ack stub ────────────────────────────────────────

#[tokio::test]
async fn partners_disclaimer_get_defaults_not_accepted() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let got = call(&app, "GET", &partners_disclaimer(&ws), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["accepted"], false,
        "default must be not accepted: {}",
        got.body
    );
    assert!(
        got.body["accepted_at"].is_null(),
        "default accepted_at must be null: {}",
        got.body
    );
}

#[tokio::test]
async fn partners_disclaimer_post_then_get_ack_is_sticky() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let posted = call(
        &app,
        "POST",
        &partners_disclaimer(&ws),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(posted.status, StatusCode::OK, "post: {}", posted.body);
    assert_eq!(posted.body["accepted"], true, "{}", posted.body);
    let accepted_at = posted.body["accepted_at"]
        .as_str()
        .unwrap_or_else(|| panic!("accepted_at: {}", posted.body))
        .to_string();
    assert!(
        !accepted_at.is_empty(),
        "POST must return accepted_at: {}",
        posted.body
    );

    let got = call(&app, "GET", &partners_disclaimer(&ws), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "get after post: {}", got.body);
    assert_eq!(got.body["accepted"], true, "{}", got.body);
    assert_eq!(
        got.body["accepted_at"].as_str(),
        Some(accepted_at.as_str()),
        "GET must return the recorded timestamp: {}",
        got.body
    );

    let again = call(
        &app,
        "POST",
        &partners_disclaimer(&ws),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "second post: {}", again.body);
    assert_eq!(again.body["accepted"], true, "{}", again.body);
    assert_eq!(
        again.body["accepted_at"].as_str(),
        Some(accepted_at.as_str()),
        "second POST must not change accepted_at: {}",
        again.body
    );
}

#[tokio::test]
async fn partners_disclaimer_ack_does_not_enqueue_or_create_worker_hosts() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let assignable_before = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(30),
    )
    .await
    .expect("list_assignable before");

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/worker-hosts"),
        Some(&jar),
        Some(json!({ "name": format!("ack-seed-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create host: {}",
        created.body
    );
    let seeded_id = created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("host.id: {}", created.body))
        .to_string();

    let before_list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/worker-hosts"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        before_list.status,
        StatusCode::OK,
        "hosts before ack: {}",
        before_list.body
    );
    let before_ids: Vec<String> = before_list.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("hosts array: {}", before_list.body))
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();

    let posted = call(
        &app,
        "POST",
        &partners_disclaimer(&ws),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(posted.status, StatusCode::OK, "post: {}", posted.body);

    let hosts = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/worker-hosts"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(hosts.status, StatusCode::OK, "hosts: {}", hosts.body);
    let host_list = hosts.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("hosts array: {}", hosts.body));
    assert!(
        host_list
            .iter()
            .all(|h| h.get("kind").and_then(|k| k.as_str()) != Some("control_plane")),
        "ack must not create a plane host: {}",
        hosts.body
    );
    let seeded = host_list
        .iter()
        .find(|h| h["id"].as_str() == Some(seeded_id.as_str()))
        .unwrap_or_else(|| panic!("seeded host {seeded_id} missing after ack: {}", hosts.body));
    assert_eq!(
        seeded["status"].as_str(),
        Some("pending"),
        "ack must not change assignable membership of the seeded id: {}",
        hosts.body
    );
    let after_ids: Vec<String> = host_list
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect();
    // HTTP list also injects this-machine boxes from other workspaces
    // (live fleet-host). Ack must not add or drop rows vs the pre-ack list.
    assert_eq!(
        after_ids, before_ids,
        "ack must not create worker_hosts: before={before_ids:?} after={after_ids:?} {}",
        hosts.body
    );
    assert!(
        after_ids.iter().any(|id| id == &seeded_id),
        "seeded host {seeded_id} missing after ack: {}",
        hosts.body
    );

    let assignable = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(30),
    )
    .await
    .expect("list_assignable");
    let before_ids: Vec<Uuid> = assignable_before.iter().map(|h| h.id).collect();
    let after_ids: Vec<Uuid> = assignable.iter().map(|h| h.id).collect();
    assert_eq!(
        after_ids, before_ids,
        "ack must not create or drop assignable hosts (cluster hosts from the shared DB may already be listed): before={assignable_before:?} after={assignable:?}"
    );

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "tasks: {}", tasks.body);
    let task_list = tasks.body["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        task_list.is_empty(),
        "ack must not enqueue fleet tasks: {}",
        tasks.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "runs: {}", runs.body);
    let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
    assert!(
        run_list.is_empty(),
        "ack must not enqueue fleet runs: {}",
        runs.body
    );
}

// ── 14. cases authz + FSM ───────────────────────────────────────────────────

fn cases_path(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/cases")
    } else {
        format!("/api/workspaces/{ws}/cases{suffix}")
    }
}

fn inbox_path(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/inbox")
    } else {
        format!("/api/workspaces/{ws}/inbox{suffix}")
    }
}

async fn create_open_case(app: &axum::Router, jar: &CookieJar, ws: &str, title: &str) -> String {
    let created = call(
        app,
        "POST",
        &cases_path(ws, ""),
        Some(jar),
        Some(json!({
            "title": title,
            "policy_md": "test policy",
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create case: {}",
        created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case id: {}", created.body))
        .to_string()
}

async fn cancel_open_case(app: &axum::Router, jar: &CookieJar, ws: &str, title: &str) -> String {
    let case_id = create_open_case(app, jar, ws, title).await;
    let cancelled = call(
        app,
        "POST",
        &cases_path(ws, &format!("/{case_id}/cancel")),
        Some(jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        cancelled.status,
        StatusCode::OK,
        "setup cancel: {}",
        cancelled.body
    );
    assert_eq!(
        cancelled.body["status"],
        "cancelled",
        "setup cancel: {}",
        cancelled.body
    );
    case_id
}

fn cancelled_event_count(body: &Value) -> usize {
    body["events"]
        .as_array()
        .map(|evs| {
            evs.iter()
                .filter(|e| e["kind"].as_str() == Some("cancelled"))
                .count()
        })
        .unwrap_or(0)
}

async fn two_tenants_and_open_case(
    app: &axum::Router,
    title: &str,
) -> (CookieJar, String, CookieJar, String, String) {
    let (_a, a_jar, a_ws) = signup_and_login(app).await;
    let (_b, b_jar, b_ws) = signup_and_login(app).await;
    let case_id = create_open_case(app, &a_jar, &a_ws, title).await;
    (a_jar, a_ws, b_jar, b_ws, case_id)
}

fn parse_next_wake_at(body: &Value) -> chrono::DateTime<chrono::Utc> {
    let raw = body["next_wake_at"]
        .as_str()
        .unwrap_or_else(|| panic!("next_wake_at must be an RFC3339 string: {body}"));
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("next_wake_at rfc3339 {raw}: {e}"))
        .with_timezone(&chrono::Utc)
}

fn assert_case_denied(out: &HttpOut, case_id: &str, title: &str, expected: StatusCode) {
    assert_eq!(
        out.status,
        expected,
        "must be {expected}, never leak a case: {} {}",
        out.status,
        out.body
    );
    if out.body.get("id").and_then(|v| v.as_str()) == Some(case_id) {
        panic!("leaked case id {case_id} in {}: {}", out.status, out.body);
    }
    if out.body.get("title").and_then(|v| v.as_str()) == Some(title) {
        panic!("leaked case title {title} in {}: {}", out.status, out.body);
    }
}

async fn assert_case_untouched(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    case_id: &str,
    title: &str,
) {
    let still = call(
        app,
        "GET",
        &cases_path(ws, &format!("/{case_id}")),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["id"], case_id, "{}", still.body);
    assert_eq!(still.body["title"], title, "{}", still.body);
    assert_eq!(
        still.body["status"],
        "open",
        "row must stay open: {}",
        still.body
    );
    let observations = still.body["context"]["observations"].as_array();
    assert!(
        observations.map(|a| a.is_empty()).unwrap_or(true),
        "context.observations must stay empty: {}",
        still.body
    );
}

#[tokio::test]
async fn cases_other_workspace_cannot_get() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (_a_jar, a_ws, b_jar, _b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "GET",
        &cases_path(&a_ws, &format!("/{case_id}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cases_other_workspace_cannot_observe() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, _b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{case_id}/observe")),
        Some(&b_jar),
        Some(json!({ "note": "should not land", "wake": false })),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::FORBIDDEN);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_other_workspace_cannot_decide() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, _b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{case_id}/decide")),
        Some(&b_jar),
        Some(json!({
            "decision": { "action": "wait", "reason": "cross-tenant" }
        })),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::FORBIDDEN);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_other_workspace_cannot_cancel() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, _b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{case_id}/cancel")),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::FORBIDDEN);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_idor_get_foreign_case_is_not_found() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (_a_jar, _a_ws, b_jar, b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "GET",
        &cases_path(&b_ws, &format!("/{case_id}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cases_idor_observe_foreign_case_is_not_found() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&b_ws, &format!("/{case_id}/observe")),
        Some(&b_jar),
        Some(json!({ "note": "should not land", "wake": false })),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::NOT_FOUND);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_idor_decide_foreign_case_is_not_found() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&b_ws, &format!("/{case_id}/decide")),
        Some(&b_jar),
        Some(json!({
            "decision": { "action": "wait", "reason": "cross-tenant" }
        })),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::NOT_FOUND);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_idor_cancel_foreign_case_is_not_found() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&b_ws, &format!("/{case_id}/cancel")),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::NOT_FOUND);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_idor_wake_foreign_case_is_not_found() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let out = call(
        &app,
        "POST",
        &cases_path(&b_ws, &format!("/{case_id}/wake")),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_case_denied(&out, &case_id, title, StatusCode::NOT_FOUND);
    assert_case_untouched(&app, &a_jar, &a_ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_other_workspace_cannot_list_or_create() {
    let app = app().await;
    let title = "tenant-a-secret-case";
    let (a_jar, a_ws, b_jar, _b_ws, case_id) =
        two_tenants_and_open_case(&app, title).await;

    let listed = call(&app, "GET", &cases_path(&a_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        listed.status,
        StatusCode::FORBIDDEN,
        "B GET A's cases: {}",
        listed.body
    );
    let leaked = listed.body["cases"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| {
            c["id"].as_str() == Some(case_id.as_str()) || c["title"].as_str() == Some(title)
        });
    assert!(
        !leaked,
        "B list must not leak A's case: {}",
        listed.body
    );

    let created = call(
        &app,
        "POST",
        &cases_path(&a_ws, ""),
        Some(&b_jar),
        Some(json!({
            "title": "tenant-b-should-not-create",
            "policy_md": "no",
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::FORBIDDEN,
        "B POST A's cases: {}",
        created.body
    );
    if created.body.get("title").and_then(|v| v.as_str()) == Some("tenant-b-should-not-create")
    {
        panic!(
            "B must not create into A's workspace: {} {}",
            created.status, created.body
        );
    }

    let still = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    let cases = still.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", still.body));
    assert_eq!(
        cases.len(),
        1,
        "A's list must still have only A's case: {}",
        still.body
    );
    assert_eq!(cases[0]["id"], case_id, "{}", still.body);
    assert_eq!(cases[0]["title"], title, "{}", still.body);
    let titles: Vec<&str> = cases.iter().filter_map(|c| c["title"].as_str()).collect();
    assert!(
        !titles.contains(&"tenant-b-should-not-create"),
        "B must not create into A's workspace: {}",
        still.body
    );
}

#[tokio::test]
async fn cases_list_own_workspace_omits_foreign_case() {
    let app = app().await;
    let a_title = "tenant-a-secret-case";
    let (_a_jar, _a_ws, b_jar, b_ws, a_case_id) =
        two_tenants_and_open_case(&app, a_title).await;
    let b_title = "tenant-b-own-case";
    let b_case_id = create_open_case(&app, &b_jar, &b_ws, b_title).await;

    let listed = call(&app, "GET", &cases_path(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let cases = listed.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", listed.body));
    let ids: Vec<&str> = cases.iter().filter_map(|c| c["id"].as_str()).collect();
    let titles: Vec<&str> = cases.iter().filter_map(|c| c["title"].as_str()).collect();
    assert!(
        ids.contains(&b_case_id.as_str()),
        "B's list must include B's case {b_case_id}: {}",
        listed.body
    );
    assert!(
        !ids.contains(&a_case_id.as_str()),
        "B's list must omit A's case {a_case_id}: {}",
        listed.body
    );
    assert!(
        !titles.contains(&a_title),
        "B's list must omit A's title: {}",
        listed.body
    );
}

#[tokio::test]
async fn cases_unauthenticated_item_routes_are_401() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let title = "needs-auth";
    let case_id = create_open_case(&app, &jar, &ws, title).await;
    let item = cases_path(&ws, &format!("/{case_id}"));

    let get = call(&app, "GET", &item, None, None).await;
    assert_eq!(get.status, StatusCode::UNAUTHORIZED, "GET: {}", get.body);
    assert_case_denied(&get, &case_id, title, StatusCode::UNAUTHORIZED);

    let observe = call(
        &app,
        "POST",
        &format!("{item}/observe"),
        None,
        Some(json!({ "note": "anon must not land", "wake": false })),
    )
    .await;
    assert_eq!(
        observe.status,
        StatusCode::UNAUTHORIZED,
        "observe: {}",
        observe.body
    );
    assert_case_denied(&observe, &case_id, title, StatusCode::UNAUTHORIZED);

    let decide = call(
        &app,
        "POST",
        &format!("{item}/decide"),
        None,
        Some(json!({ "decision": { "action": "wait", "reason": "anon" } })),
    )
    .await;
    assert_eq!(
        decide.status,
        StatusCode::UNAUTHORIZED,
        "decide: {}",
        decide.body
    );
    assert_case_denied(&decide, &case_id, title, StatusCode::UNAUTHORIZED);

    let cancel = call(
        &app,
        "POST",
        &format!("{item}/cancel"),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        cancel.status,
        StatusCode::UNAUTHORIZED,
        "cancel: {}",
        cancel.body
    );
    assert_case_denied(&cancel, &case_id, title, StatusCode::UNAUTHORIZED);

    let wake = call(&app, "POST", &format!("{item}/wake"), None, Some(json!({}))).await;
    assert_eq!(wake.status, StatusCode::UNAUTHORIZED, "wake: {}", wake.body);
    assert_case_denied(&wake, &case_id, title, StatusCode::UNAUTHORIZED);

    assert_case_untouched(&app, &jar, &ws, &case_id, title).await;
}

#[tokio::test]
async fn cases_observe_after_cancel_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = cancel_open_case(&app, &jar, &ws, "observe-after-cancel").await;

    let observed = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "after cancel", "wake": false })),
    )
    .await;
    assert_eq!(
        observed.status,
        StatusCode::BAD_REQUEST,
        "observe on cancelled: {}",
        observed.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "cancelled", "{}", got.body);
}

#[tokio::test]
async fn cases_decide_after_cancel_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = cancel_open_case(&app, &jar, &ws, "decide-after-cancel").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({ "decision": { "action": "wait" } })),
    )
    .await;
    assert_eq!(
        decided.status,
        StatusCode::BAD_REQUEST,
        "decide on cancelled: {}",
        decided.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "cancelled", "{}", got.body);
}

#[tokio::test]
async fn cases_wake_after_cancel_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = cancel_open_case(&app, &jar, &ws, "wake-after-cancel").await;

    let woke = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        woke.status,
        StatusCode::BAD_REQUEST,
        "wake on cancelled: {}",
        woke.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "cancelled", "{}", got.body);
}

#[tokio::test]
async fn cases_cancel_cancelled_is_noop() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = cancel_open_case(&app, &jar, &ws, "cancel-again").await;

    let again = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/cancel")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "cancel again: {}", again.body);
    assert_eq!(again.body["status"], "cancelled", "{}", again.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "cancelled", "{}", got.body);
    assert!(
        got.body["next_wake_at"].is_null(),
        "re-cancel leftover next_wake_at: {}",
        got.body
    );
    assert_eq!(
        cancelled_event_count(&got.body),
        1,
        "early-return must not append a second cancelled event: {}",
        got.body
    );
}

#[tokio::test]
async fn cases_cancel_completed_is_noop() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "cancel-completed").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "complete", "reason": "done" }
        })),
    )
    .await;
    assert_eq!(
        decided.status,
        StatusCode::OK,
        "setup complete: {}",
        decided.body
    );
    assert_eq!(decided.body["status"], "completed", "{}", decided.body);

    let cancelled = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/cancel")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(
        cancelled.body["status"],
        "completed",
        "cancel must not flip completed: {}",
        cancelled.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "completed", "{}", got.body);
    assert_eq!(
        cancelled_event_count(&got.body),
        0,
        "cancel of completed must not append cancelled: {}",
        got.body
    );
}

#[tokio::test]
async fn cases_cancel_unknown_id_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let missing = Uuid::new_v4();

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{missing}/cancel")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);
    assert!(
        out.body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "404 must publish ErrorBody.error: {}",
        out.body
    );
}

#[tokio::test]
async fn cases_observe_empty_note_is_required() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-note").await;

    for note in ["", "   "] {
        let out = call(
            &app,
            "POST",
            &cases_path(&ws, &format!("/{case_id}/observe")),
            Some(&jar),
            Some(json!({ "note": note, "wake": false })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "note={note:?}: {}",
            out.body
        );
    }

    let still = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["status"], "open", "{}", still.body);
    let events = still.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        !kinds.contains(&"observed"),
        "rejected observe must not append an event: {kinds:?}"
    );
}

#[tokio::test]
async fn cases_observe_omitted_note_is_not_ok() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-omitted-note").await;

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "wake": false })),
    )
    .await;
    assert_ne!(
        out.status,
        StatusCode::OK,
        "omitted note must not observe: {}",
        out.body
    );

    let still = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["status"], "open", "{}", still.body);
    let events = still.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        !kinds.contains(&"observed"),
        "omitted note must not append an event: {kinds:?}"
    );
}

#[tokio::test]
async fn cases_observe_single_nonwhitespace_char_is_200() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-one-char").await;

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "x", "wake": false })),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "note=x: {}", out.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"observed"),
        "single-char note must append observed: {kinds:?}"
    );
}

#[tokio::test]
async fn cases_observe_trims_note_before_persist() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-trim-note").await;

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "  slack replied  ", "wake": false })),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let observed = events
        .iter()
        .find(|e| e["kind"].as_str() == Some("observed"))
        .unwrap_or_else(|| panic!("observed event missing: {}", got.body));
    assert_eq!(
        observed["payload"]["note"],
        "slack replied",
        "event note must be trimmed: {}",
        got.body
    );
    let observations = got.body["context"]["observations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ctx_note = observations
        .iter()
        .find_map(|o| o["note"].as_str())
        .unwrap_or("");
    assert_eq!(
        ctx_note, "slack replied",
        "context observation must be trimmed: {}",
        got.body
    );
}

#[tokio::test]
async fn cases_observe_too_long() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-too-long").await;

    let note = "x".repeat(4001);
    assert_eq!(note.chars().count(), 4001);

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": note, "wake": false })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "4001-char observe must 400: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("too long") || err.contains("4000"),
        "expected note-too-long error, got {err:?} body={}",
        out.body
    );

    let still = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["status"], "open", "{}", still.body);
    let events = still.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        !kinds.contains(&"observed"),
        "rejected observe must not append an event: {kinds:?}"
    );
}

#[tokio::test]
async fn cases_wait_seconds_zero_clamps_to_min() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "wait-seconds-zero").await;

    let t0 = chrono::Utc::now();
    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "wait",
                "wait_seconds": 0,
                "reason": "clamp min"
            }
        })),
    )
    .await;
    let t1 = chrono::Utc::now();
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);

    let next = parse_next_wake_at(&decided.body);
    let min = t0 + chrono::Duration::seconds(5);
    let max = t1 + chrono::Duration::seconds(7);
    assert!(
        next >= min,
        "wait_seconds=0 must yield next_wake >= now+5s: next={next} min={min} body={}",
        decided.body
    );
    assert!(
        next <= max,
        "wait_seconds=0 must clamp to the 5s floor, not default/huge: next={next} max={max} body={}",
        decided.body
    );
}

#[tokio::test]
async fn cases_wait_seconds_huge_clamps_to_max() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "wait-seconds-huge").await;

    let huge: i64 = 1_000_000_000_000; // 10^12 — would overflow chrono if unclamped
    let t0 = chrono::Utc::now();
    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "wait",
                "wait_seconds": huge,
                "reason": "clamp max"
            }
        })),
    )
    .await;
    let t1 = chrono::Utc::now();
    assert_eq!(
        decided.status,
        StatusCode::OK,
        "huge wait_seconds must not overflow: {}",
        decided.body
    );
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);

    let next = parse_next_wake_at(&decided.body);
    let max_wait = fleet_server::cases::MAX_WAIT_SECS;
    let min = t0 + chrono::Duration::seconds(max_wait);
    let max = t1 + chrono::Duration::seconds(max_wait + 2);
    assert!(
        next >= min,
        "10^12 must clamp to MAX_WAIT_SECS, not collapse: next={next} min={min} body={}",
        decided.body
    );
    assert!(
        next <= max,
        "10^12 must clamp (no overflow / far-future abuse): next={next} max={max} body={}",
        decided.body
    );
    assert!(
        next <= t1 + chrono::Duration::days(8),
        "clamped wake must be days away, not 10^12s: next={next} body={}",
        decided.body
    );
}

#[tokio::test]
async fn cases_wake_owner_is_200_waiting_with_wake_requested() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "wake-owner").await;

    let woke = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(woke.status, StatusCode::OK, "{}", woke.body);
    assert_eq!(woke.body["status"], "waiting", "{}", woke.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "waiting", "{}", got.body);
    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"wake_requested"),
        "events must include kind wake_requested: {kinds:?} body={}",
        got.body
    );
}

#[tokio::test]
async fn cases_wake_unknown_id_is_404() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let missing = Uuid::new_v4();

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{missing}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);
    assert_eq!(
        out.body["error"].as_str(),
        Some("case not found"),
        "unknown case must 404 case not found: {}",
        out.body
    );
}

#[tokio::test]
async fn cases_wake_invited_member_is_200() {
    let app = app().await;
    let (_email, owner_jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &owner_jar, &ws, "wake-member").await;
    let member_jar = invite_existing_member(&app, &owner_jar, &ws).await;

    let woke = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/wake")),
        Some(&member_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(woke.status, StatusCode::OK, "{}", woke.body);
    assert_eq!(woke.body["status"], "waiting", "{}", woke.body);
}

#[tokio::test]
async fn cases_wake_unauthenticated_is_401() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "wake-anon").await;

    let out = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/wake")),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "no-cookie wake must be 401 not 403: {} {}",
        out.status,
        out.body
    );
}

#[tokio::test]
async fn cases_decide_unknown_action_waits_with_last_error() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": "unknown-action",
            "policy_md": "test policy",
            "wake_in_s": 5
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create case: {}",
        created.body
    );
    let case_id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case id: {}", created.body))
        .to_string();
    let before_count = created.body["decision_count"].as_i64().unwrap_or(0);

    let t0 = chrono::Utc::now();
    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "not_a_real_action", "reason": "probe" }
        })),
    )
    .await;
    let t1 = chrono::Utc::now();
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    let err = decided.body["last_error"].as_str().unwrap_or("");
    assert!(
        !err.is_empty(),
        "unknown action must set a non-empty last_error: {}",
        decided.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "waiting", "{}", got.body);
    let got_err = got.body["last_error"].as_str().unwrap_or("");
    assert!(
        !got_err.is_empty(),
        "GET last_error must persist: {}",
        got.body
    );

    let next = parse_next_wake_at(&got.body);
    let wait = fleet_server::cases::DEFAULT_WAIT_SECS;
    let min = t0 + chrono::Duration::seconds(wait);
    let max = t1 + chrono::Duration::seconds(wait + 2);
    assert!(
        next >= min,
        "unknown action must schedule next_wake ≈ now+DEFAULT_WAIT_SECS: next={next} min={min} body={}",
        got.body
    );
    assert!(
        next <= max,
        "unknown action next_wake must be now+DEFAULT_WAIT_SECS, not the create wake: next={next} max={max} body={}",
        got.body
    );

    let after_count = got.body["decision_count"].as_i64().unwrap_or(0);
    assert!(
        after_count > before_count,
        "unknown action must bump decision_count: before={before_count} after={after_count} body={}",
        got.body
    );

    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"error"),
        "events must include kind error: {kinds:?} body={}",
        got.body
    );
}

#[tokio::test]
async fn cases_decide_uppercase_or_padded_wait_is_not_unknown() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for action in ["WAIT", " Wait "] {
        let case_id = create_open_case(&app, &jar, &ws, "wait-normalize").await;
        let decided = call(
            &app,
            "POST",
            &cases_path(&ws, &format!("/{case_id}/decide")),
            Some(&jar),
            Some(json!({
                "decision": { "action": action }
            })),
        )
        .await;
        assert_eq!(
            decided.status,
            StatusCode::OK,
            "action={action:?}: {}",
            decided.body
        );
        assert_eq!(
            decided.body["status"],
            "waiting",
            "action={action:?}: {}",
            decided.body
        );
        assert!(
            decided.body["last_error"].is_null(),
            "trim+lowercase wait must not be unknown: action={action:?} body={}",
            decided.body
        );
    }
}

#[tokio::test]
async fn cases_decide_empty_action_recovers_to_waiting() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for action in ["", "   "] {
        let case_id = create_open_case(&app, &jar, &ws, "empty-action").await;
        let decided = call(
            &app,
            "POST",
            &cases_path(&ws, &format!("/{case_id}/decide")),
            Some(&jar),
            Some(json!({
                "decision": { "action": action }
            })),
        )
        .await;
        assert_eq!(
            decided.status,
            StatusCode::OK,
            "action={action:?} must not 400: {}",
            decided.body
        );
        assert_eq!(
            decided.body["status"],
            "waiting",
            "action={action:?}: {}",
            decided.body
        );
        let err = decided.body["last_error"].as_str().unwrap_or("");
        assert!(
            !err.is_empty(),
            "empty/whitespace action must set last_error, not silent wait: action={action:?} body={}",
            decided.body
        );
    }
}

#[tokio::test]
async fn cases_decide_complete_sets_status_and_completed_event() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "decide-complete").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "complete", "reason": "done" }
        })),
    )
    .await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "completed", "{}", decided.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "completed", "{}", got.body);
    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"completed"),
        "events must include kind completed: {kinds:?} body={}",
        got.body
    );
}

#[tokio::test]
async fn cases_decide_fail_sets_status_last_error_and_failed_event() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "decide-fail").await;

    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "fail", "reason": "boom" }
        })),
    )
    .await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "failed", "{}", decided.body);
    assert_eq!(
        decided.body["last_error"].as_str().unwrap_or(""),
        "boom",
        "last_error must be the fail reason: {}",
        decided.body
    );

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "failed", "{}", got.body);
    assert_eq!(got.body["last_error"], "boom", "{}", got.body);
    let events = got.body["events"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"failed"),
        "events must include kind failed: {kinds:?} body={}",
        got.body
    );
}

#[tokio::test]
async fn cases_list_status_filter_open_excludes_cancelled() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let open_id = create_open_case(&app, &jar, &ws, "stay-open").await;
    let cancel_id = create_open_case(&app, &jar, &ws, "to-cancel").await;

    let cancelled = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{cancel_id}/cancel")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["status"], "cancelled");

    let open_list = call(
        &app,
        "GET",
        &format!("{}?status=open", cases_path(&ws, "")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(open_list.status, StatusCode::OK, "{}", open_list.body);
    let open_ids: Vec<&str> = open_list.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", open_list.body))
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert!(
        open_ids.contains(&open_id.as_str()),
        "open list missing {open_id}: {}",
        open_list.body
    );
    assert!(
        !open_ids.contains(&cancel_id.as_str()),
        "open list must not include cancelled {cancel_id}: {}",
        open_list.body
    );

    let cancelled_list = call(
        &app,
        "GET",
        &format!("{}?status=cancelled", cases_path(&ws, "")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        cancelled_list.status,
        StatusCode::OK,
        "{}",
        cancelled_list.body
    );
    let cancelled_ids: Vec<&str> = cancelled_list.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", cancelled_list.body))
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert!(
        cancelled_ids.contains(&cancel_id.as_str()),
        "cancelled list missing {cancel_id}: {}",
        cancelled_list.body
    );
}

// ── 15. inbox HTTP authz ────────────────────────────────────────────────────

#[tokio::test]
async fn inbox_other_workspace_poll_is_403() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("ws");
    let key = format!("iso-poll-{}/app#1", Uuid::new_v4());
    seed_inbox_ticket(&state, a_ws_id, &key, "a open github", false).await;

    let poll = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/poll"),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(poll.status, StatusCode::FORBIDDEN, "{}", poll.body);

    let leftover = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 1, "no new stub: {}", leftover.body);
    assert_eq!(tickets[0]["key"].as_str(), Some(key.as_str()), "{}", leftover.body);
    assert_eq!(
        tickets[0]["state"].as_str(),
        Some("open"),
        "seeded row must stay open: {}",
        leftover.body
    );
    assert!(
        tickets[0]["run_id"].is_null(),
        "run_id must stay null: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_other_workspace_finish_is_403() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("ws");
    let key = format!("iso-finish-{}/app#1", Uuid::new_v4());
    let stub = format!("iso-finish-{}/app#stub", Uuid::new_v4());
    seed_inbox_ticket(&state, a_ws_id, &key, "a open github", false).await;

    let finish = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/finish"),
        Some(&b_jar),
        Some(json!({ "provider": "github", "key": stub })),
    )
    .await;
    assert_eq!(finish.status, StatusCode::FORBIDDEN, "{}", finish.body);

    let leftover = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 1, "no new stub: {}", leftover.body);
    assert_eq!(tickets[0]["key"].as_str(), Some(key.as_str()), "{}", leftover.body);
    assert_eq!(
        tickets[0]["state"].as_str(),
        Some("open"),
        "seeded row must stay open: {}",
        leftover.body
    );
    assert!(
        tickets[0]["run_id"].is_null(),
        "run_id must stay null: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_other_workspace_monitor_post_is_403() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;

    let monitor = call(
        &app,
        "POST",
        &inbox_path(&a_ws, ""),
        Some(&b_jar),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(monitor.status, StatusCode::FORBIDDEN, "{}", monitor.body);

    let leftover = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let enabled = leftover.body["monitor"]
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        !enabled,
        "monitor.enabled must stay unset/false: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_unauthenticated_poll_and_finish_are_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let poll = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(poll.status, StatusCode::UNAUTHORIZED, "{}", poll.body);

    let finish = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        None,
        Some(json!({ "provider": "github", "key": "acme/app#1" })),
    )
    .await;
    assert_eq!(finish.status, StatusCode::UNAUTHORIZED, "{}", finish.body);
}

#[tokio::test]
async fn inbox_removed_member_poll_finish_is_403() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("ws");
    let key = format!("iso-removed-{}/app#1", Uuid::new_v4());
    seed_inbox_ticket(&state, a_ws_id, &key, "a open github", false).await;
    let b_jar = invite_existing_member(&app, &a_jar, &a_ws).await;
    let (b_id, _) = authed_user_and_workspace(&app, &b_jar, &a_ws).await;
    let removed = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{a_ws}/members/{b_id}"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::OK, "remove member: {}", removed.body);

    let poll = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/poll"),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(poll.status, StatusCode::FORBIDDEN, "{}", poll.body);

    let finish = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/finish"),
        Some(&b_jar),
        Some(json!({ "provider": "github", "key": key })),
    )
    .await;
    assert_eq!(finish.status, StatusCode::FORBIDDEN, "{}", finish.body);

    let leftover = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 1, "no new stub: {}", leftover.body);
    assert_eq!(tickets[0]["key"].as_str(), Some(key.as_str()), "{}", leftover.body);
    assert_eq!(
        tickets[0]["state"].as_str(),
        Some("open"),
        "removed member must not claim A's row: {}",
        leftover.body
    );
    assert!(
        tickets[0]["run_id"].is_null(),
        "run_id must stay null: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_finish_unknown_provider_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({ "provider": "not_a_real_provider", "key": "FAKE-KEY-000" })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "unknown inbox provider must be 400, not launch: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "unknown provider must not return ok: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "unknown provider must not return run_id: {}",
        out.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert!(
        tickets.iter().all(|t| {
            t["provider"].as_str() != Some("not_a_real_provider")
                && t["key"].as_str() != Some("FAKE-KEY-000")
        }),
        "unknown provider must not seed a stub: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_finish_unseen_github_key_is_404_without_create_flag() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let key = format!("acme-{}/app#999", Uuid::new_v4());

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({ "provider": "github", "key": key })),
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);
    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    let tickets = leftover.body["tickets"].as_array().cloned().unwrap_or_default();
    assert!(
        tickets.iter().all(|t| t["key"].as_str() != Some(key.as_str())),
        "must not stub-create: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_finish_unseen_github_key_seeds_when_create_if_missing() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let key = format!("acme-{}/app#999", Uuid::new_v4());

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({
            "provider": "github",
            "key": key,
            "create_if_missing": true
        })),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["ok"], true, "{}", out.body);
    let run_id = out.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("run_id: {}", out.body));
    assert!(!run_id.is_empty(), "run_id: {}", out.body);

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 1, "seeded stub: {}", leftover.body);
    assert_eq!(
        tickets[0]["provider"].as_str(),
        Some("github"),
        "{}",
        leftover.body
    );
    assert_eq!(tickets[0]["key"].as_str(), Some(key.as_str()), "{}", leftover.body);
    assert_eq!(
        tickets[0]["state"].as_str(),
        Some("launched"),
        "GET must show launched: {}",
        leftover.body
    );
    assert_eq!(
        tickets[0]["run_id"].as_str(),
        Some(run_id),
        "{}",
        leftover.body
    );

    let again = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({ "provider": "github", "key": key })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert_eq!(
        again.body["error"].as_str(),
        Some("ticket already launched"),
        "{}",
        again.body
    );
    assert_eq!(
        again.body["code"].as_str(),
        Some("already_launched"),
        "{}",
        again.body
    );
}

#[tokio::test]
async fn inbox_finish_ambiguous_key_without_provider_is_400() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let key = format!("ENG-{}", Uuid::new_v4());
    fleet_db::repos::tickets::upsert_inbox(
        &state.db,
        &fleet_db::repos::tickets::UpsertInbox {
            workspace_id: ws_id,
            provider: "jira".into(),
            external_key: key.clone(),
            title: "jira card".into(),
            url: None,
            summary: "jira card".into(),
            provider_closed: false,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("seed jira:{key}: {e}"));
    fleet_db::repos::tickets::upsert_inbox(
        &state.db,
        &fleet_db::repos::tickets::UpsertInbox {
            workspace_id: ws_id,
            provider: "linear".into(),
            external_key: key.clone(),
            title: "linear issue".into(),
            url: None,
            summary: "linear issue".into(),
            provider_closed: false,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("seed linear:{key}: {e}"));

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({ "key": key })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "ambiguous key must not return ok: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "ambiguous key must not return run_id: {}",
        out.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 2, "both seeded rows stay: {}", leftover.body);
    assert!(
        tickets.iter().all(|t| {
            t["key"].as_str() == Some(key.as_str())
                && t["state"].as_str() == Some("open")
                && t["run_id"].is_null()
        }),
        "ambiguous finish must not launch either row: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_finish_empty_body_picks_the_only_open_ticket() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let key = format!("iso-empty-{}/app#1", Uuid::new_v4());
    seed_inbox_ticket(&state, ws_id, &key, "only open ticket", false).await;

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/finish"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["ok"], true, "{}", out.body);
    let run_id = out.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("run_id: {}", out.body));
    assert!(!run_id.is_empty(), "run_id: {}", out.body);
    assert_eq!(
        out.body["ticket"]["key"].as_str(),
        Some(key.as_str()),
        "{}",
        out.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert_eq!(tickets.len(), 1, "{}", leftover.body);
    assert_eq!(tickets[0]["key"].as_str(), Some(key.as_str()), "{}", leftover.body);
    assert_eq!(
        tickets[0]["state"].as_str(),
        Some("launched"),
        "GET must show launched: {}",
        leftover.body
    );
    assert_eq!(
        tickets[0]["run_id"].as_str(),
        Some(run_id),
        "{}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_poll_without_monitor_returns_skipped_no_monitor() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["skipped"], true, "{}", out.body);
    assert_eq!(out.body["reason"], "no monitor", "{}", out.body);

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    assert!(
        leftover.body["monitor"].is_null(),
        "poll must not invent a monitor row: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_poll_disabled_monitor_returns_skipped_disabled() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["skipped"], true, "{}", out.body);
    assert_eq!(out.body["reason"], "disabled", "{}", out.body);

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    assert_eq!(
        leftover.body["monitor"]["enabled"], false,
        "disabled skip must leave the row off: {}",
        leftover.body
    );
    assert!(
        leftover.body["monitor"]["last_poll_at"].is_null(),
        "disabled skip must not mark_polled: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_poll_enabled_no_connections_is_200_seen_zero() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let enabled = sqlx::query("UPDATE ticket_monitors SET enabled = true WHERE workspace_id = $1")
        .bind(ws_id)
        .execute(&state.db)
        .await
        .expect("enable monitor with no integrations");
    assert_eq!(enabled.rows_affected(), 1, "must flip the existing row on");

    let polled = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    assert_eq!(polled.body["watching"], json!([]), "{}", polled.body);
    assert_eq!(polled.body["seen"], 0, "{}", polled.body);
    assert_ne!(
        polled.body.get("skipped"),
        Some(&json!(true)),
        "enabled empty watching must walk mark_polled, not skip: {}",
        polled.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    assert_eq!(
        leftover.body["monitor"]["enabled"], true,
        "{}",
        leftover.body
    );
    assert!(
        leftover.body["monitor"]["last_poll_at"].is_string(),
        "enabled poll must mark_polled: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_poll_connected_empty_creds_puts_not_connected_in_errors() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    fleet_db::repos::integrations::upsert(
        &state.db,
        &fleet_db::repos::integrations::UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: ws_id,
            provider: "slack".into(),
            account_label: Some("empty".into()),
            config: json!({}),
            credentials: json!({}),
            connected_by: None,
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("seed empty slack");

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);

    let polled = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    let errors = polled.body["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("poll.errors must be a list: {}", polled.body));
    assert!(
        !errors.is_empty(),
        "connected empty creds must produce errors: {}",
        polled.body
    );
    let joined = errors
        .iter()
        .filter_map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        joined.to_ascii_lowercase().contains("slack"),
        "errors join must name slack: {joined}"
    );
}

#[tokio::test]
async fn inbox_get_after_poll_monitor_last_error_joins_errors() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    fleet_db::repos::integrations::upsert(
        &state.db,
        &fleet_db::repos::integrations::UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: ws_id,
            provider: "slack".into(),
            account_label: Some("empty".into()),
            config: json!({}),
            credentials: json!({}),
            connected_by: None,
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("seed empty slack");

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);

    let polled = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    let errors = polled.body["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("poll.errors must be a list: {}", polled.body));
    let joined = errors
        .iter()
        .filter_map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        !joined.is_empty(),
        "need errors to lock last_error join: {}",
        polled.body
    );

    let inbox = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
    let last_error = inbox.body["monitor"]["last_error"].as_str().unwrap_or("");
    assert_eq!(
        last_error, joined,
        "monitor.last_error must equal mark_polled join: inbox={} poll={}",
        inbox.body, polled.body
    );
}

#[tokio::test]
async fn inbox_poll_watching_skips_disconnected_github() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "slack").await;
    seed_inbox_connection(&state, ws_id, "github").await;
    fleet_db::repos::integrations::disconnect(&state.db, ws_id, "github")
        .await
        .expect("disconnect github");

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": true, "follow_connected": true })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);

    let polled = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    let watching = polled.body["watching"]
        .as_array()
        .unwrap_or_else(|| panic!("watching: {}", polled.body));
    let watching: Vec<&str> = watching.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        watching.iter().any(|p| *p == "slack"),
        "watching must include slack: {}",
        polled.body
    );
    assert!(
        watching.iter().all(|p| *p != "github"),
        "disconnected github must not be watched: {}",
        polled.body
    );
    let errors = polled.body["errors"].as_array().cloned().unwrap_or_default();
    for e in &errors {
        let low = e.as_str().unwrap_or("").to_ascii_lowercase();
        assert!(
            !(low.contains("github") && low.contains("not connected")),
            "errors must not report github not_connected: {e}"
        );
    }
}

#[tokio::test]
async fn inbox_monitor_defaults_auto_launch() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let first = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(
        first.body["auto_launch"], false,
        "first monitor with omitted auto_launch must default false: {}",
        first.body
    );
}

#[tokio::test]
async fn inbox_monitor_omitted_auto_launch_and_jira_jql_preserves() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let saved = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": false,
            "auto_launch": false,
            "jira_jql": "project = ENG"
        })),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["auto_launch"], false);
    assert_eq!(saved.body["jira_jql"], "project = ENG");

    let again = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(
        again.body["auto_launch"], false,
        "omitted auto_launch must not wipe a saved false: {}",
        again.body
    );
    assert_eq!(
        again.body["jira_jql"], "project = ENG",
        "omitted jira_jql must keep the saved filter: {}",
        again.body
    );

    let listed = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["monitor"]["auto_launch"], false,
        "GET after omit POST must still show auto_launch false: {}",
        listed.body
    );
    assert_eq!(
        listed.body["monitor"]["jira_jql"], "project = ENG",
        "GET after omit POST must still show saved jira_jql: {}",
        listed.body
    );
}

#[tokio::test]
async fn inbox_monitor_omitted_follow_connected_preserves_false() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let saved = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": false,
            "follow_connected": false
        })),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["follow_connected"], false);

    let again = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(
        again.body["follow_connected"], false,
        "omitted follow_connected (stop_monitor) must keep saved false: {}",
        again.body
    );

    let listed = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["monitor"]["follow_connected"], false,
        "GET after omit POST must still show follow_connected false: {}",
        listed.body
    );
}

#[tokio::test]
async fn inbox_monitor_empty_jira_jql_clears_saved_filter() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let saved = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": false,
            "jira_jql": "project = ENG"
        })),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["jira_jql"], "project = ENG");

    let cleared = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": false,
            "jira_jql": ""
        })),
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert!(
        cleared.body["jira_jql"].is_null(),
        "empty jira_jql must drop the saved filter: {}",
        cleared.body
    );
}

#[tokio::test]
async fn inbox_monitor_empty_providers_does_not_snapshot_connected() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert!(
        started.body["providers"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "omitted providers must stay empty allowlist, not snapshot connected: {}",
        started.body
    );
    let watching = started.body["watching"]
        .as_array()
        .unwrap_or_else(|| panic!("watching: {}", started.body));
    let watching: Vec<&str> = watching.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        watching.iter().any(|p| *p == "github"),
        "empty allowlist + follow_connected must watch connected github: {}",
        started.body
    );
}

#[tokio::test]
async fn inbox_monitor_follow_connected_watches_whatever_is_connected() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;
    seed_inbox_connection(&state, ws_id, "slack").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert_eq!(started.body["follow_connected"], true);
    assert_eq!(started.body["watching"], json!(["github", "slack"]));
    assert_eq!(started.body["providers"], json!([]));

    let listed = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["monitor"]["follow_connected"], true);
    assert_eq!(listed.body["monitor"]["watching"], json!(["github", "slack"]));
    assert_eq!(listed.body["monitor"]["providers"], json!([]));
}

#[tokio::test]
async fn inbox_monitor_enabled_without_connection_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert!(
        out.body["error"]
            .as_str()
            .is_some_and(|e| e.contains("no listen source")),
        "enabled with no connection must 400 no-listen-source: {}",
        out.body
    );
}

#[tokio::test]
async fn inbox_monitor_pinned_watching_is_intersection() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;
    seed_inbox_connection(&state, ws_id, "slack").await;

    let pinned = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": false,
            "follow_connected": false,
            "providers": ["github"]
        })),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::OK, "{}", pinned.body);
    assert_eq!(pinned.body["watching"], json!(["github"]));
    assert_eq!(pinned.body["providers"], json!(["github"]));
}

#[tokio::test]
async fn inbox_monitor_follow_connected_picks_up_app_connected_after_start() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert_eq!(started.body["watching"], json!(["github"]));
    assert_eq!(started.body["providers"], json!([]));

    seed_inbox_connection(&state, ws_id, "slack").await;

    let listed = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["monitor"]["watching"],
        json!(["github", "slack"])
    );
    assert_eq!(listed.body["monitor"]["providers"], json!([]));
}

fn github_issue_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "action": "opened",
        "issue": {
            "number": 7,
            "title": "Hook ticket",
            "html_url": "https://github.com/acme/app/issues/7",
            "body": "from hook",
            "state": "open"
        },
        "repository": { "full_name": "acme/app" }
    }))
    .expect("github body")
}

fn slack_message_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "event": {
            "type": "message",
            "channel": "C7",
            "ts": "1700000000.000001",
            "text": "hook message"
        }
    }))
    .expect("slack body")
}

async fn seed_inbox_connection(state: &AppState, workspace_id: Uuid, provider: &str) {
    let credentials = state
        .integrations
        .credentials_key
        .encrypt(&json!({ "token": "test-not-used" }))
        .expect("encrypt");
    fleet_db::repos::integrations::upsert(
        &state.db,
        &fleet_db::repos::integrations::UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: provider.into(),
            account_label: Some("test".into()),
            config: json!({}),
            credentials,
            connected_by: None,
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("seed connection");
    let client_secret = state
        .integrations
        .credentials_key
        .encrypt(&json!({ "webhook_secret": TEST_WEBHOOK_SECRET }))
        .expect("encrypt oauth app");
    fleet_db::repos::integrations::upsert_oauth_app(
        &state.db,
        workspace_id,
        provider,
        "test-client",
        &client_secret,
        None,
        chrono::Utc::now(),
    )
    .await
    .expect("seed oauth app webhook_secret");
}

fn hmac_sha256_hex(secret: &str, data: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

fn github_webhook_headers(body: &[u8]) -> Vec<(String, String)> {
    let sig = hmac_sha256_hex(TEST_WEBHOOK_SECRET, body);
    vec![
        ("x-github-event".into(), "issues".into()),
        ("x-hub-signature-256".into(), format!("sha256={sig}")),
    ]
}

fn slack_webhook_headers(body: &[u8]) -> Vec<(String, String)> {
    let ts = chrono::Utc::now().timestamp().to_string();
    let mut msg = Vec::from(b"v0:");
    msg.extend_from_slice(ts.as_bytes());
    msg.push(b':');
    msg.extend_from_slice(body);
    let sig = format!("v0={}", hmac_sha256_hex(TEST_WEBHOOK_SECRET, &msg));
    vec![
        ("x-slack-request-timestamp".into(), ts),
        ("x-slack-signature".into(), sig),
    ]
}

fn header_pairs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[tokio::test]
async fn inbox_webhook_unknown_token_is_401() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": true,
            "follow_connected": true,
            "auto_launch": false
        })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert!(
        !hook_token_from_monitor(&started.body).is_empty(),
        "need a live hook_token so a stranger path is not the monitor: {}",
        started.body
    );

    let bad = call_raw(
        &app,
        "POST",
        "/api/integrations/webhooks/github/not-the-token",
        &[("x-github-event", "issues")],
        github_issue_body(),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert!(
        tickets.is_empty(),
        "wrong hook_token must not upsert: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_webhook_lists_upserted_ticket() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": true,
            "follow_connected": true,
            "auto_launch": false
        })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let token = hook_token_from_monitor(&started.body);
    assert!(!token.is_empty());

    let body = github_issue_body();
    let hdrs = github_webhook_headers(&body);
    let ok = call_raw(
        &app,
        "POST",
        &format!("/api/integrations/webhooks/github/{token}"),
        &header_pairs(&hdrs),
        body,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["ingested"], 1, "{}", ok.body);

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert!(
        tickets.iter().any(|t| {
            t["provider"].as_str() == Some("github") && t["key"].as_str() == Some("acme/app#7")
        }),
        "GET /inbox must list the upserted github issue: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_webhook_pinned_slack_still_ingests_slack() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "slack").await;

    let started = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": true,
            "follow_connected": true,
            "auto_launch": false
        })),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let token = hook_token_from_monitor(&started.body);
    assert!(!token.is_empty());

    let pin = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "enabled": true,
            "follow_connected": false,
            "providers": ["slack"],
            "auto_launch": false
        })),
    )
    .await;
    assert_eq!(pin.status, StatusCode::OK, "{}", pin.body);

    let body = slack_message_body();
    let hdrs = slack_webhook_headers(&body);
    let slack = call_raw(
        &app,
        "POST",
        &format!("/api/integrations/webhooks/slack/{token}"),
        &header_pairs(&hdrs),
        body,
    )
    .await;
    assert_eq!(slack.status, StatusCode::OK, "{}", slack.body);
    assert_eq!(
        slack.body["ingested"], 1,
        "pinned slack must still ingest slack: {}",
        slack.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert!(
        tickets.iter().any(|t| {
            t["provider"].as_str() == Some("slack")
                && t["key"].as_str() == Some("C7:1700000000.000001")
        }),
        "GET /inbox must list the upserted slack event: {}",
        leftover.body
    );
}

#[tokio::test]
async fn inbox_webhook_disabled_monitor_ingests_zero() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    seed_inbox_connection(&state, ws_id, "github").await;

    let stopped = call(
        &app,
        "POST",
        &inbox_path(&ws, ""),
        Some(&jar),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.body);
    let token = hook_token_from_monitor(&stopped.body);
    assert!(!token.is_empty());

    let body = github_issue_body();
    let hdrs = github_webhook_headers(&body);
    let hook = call_raw(
        &app,
        "POST",
        &format!("/api/integrations/webhooks/github/{token}"),
        &header_pairs(&hdrs),
        body,
    )
    .await;
    assert_eq!(hook.status, StatusCode::OK, "{}", hook.body);
    assert_eq!(
        hook.body["ingested"], 0,
        "disabled monitor must not ingest: {}",
        hook.body
    );

    let leftover = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(leftover.status, StatusCode::OK, "{}", leftover.body);
    let tickets = leftover.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {}", leftover.body));
    assert!(
        tickets.is_empty(),
        "disabled monitor must not upsert: {}",
        leftover.body
    );
}

fn workforce(ws: &str, suffix: &str) -> String {
    format!("/api/workspaces/{ws}/workforce{suffix}")
}

#[tokio::test]
async fn post_workforce_goal_persists_acting_case_in_open_goals() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &workforce(&ws, "/goals"),
        Some(&jar),
        Some(json!({
            "title": "Ship the weather app",
            "goal_md": "Done when the PR is merged."
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let goal_id = created.body["goal"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("goal.id: {}", created.body))
        .to_string();
    let case_id = created.body["goal"]["case_id"]
        .as_str()
        .unwrap_or_else(|| panic!("goal.case_id: {}", created.body))
        .to_string();
    assert_eq!(
        created.body["goal"]["title"].as_str(),
        Some("Ship the weather app"),
        "{}",
        created.body
    );
    assert_eq!(
        created.body["goal"]["status"].as_str(),
        Some("acting"),
        "{}",
        created.body
    );

    let listed = call(&app, "GET", &workforce(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let open_goals = listed.body["open_goals"]
        .as_array()
        .unwrap_or_else(|| panic!("open_goals: {}", listed.body));
    let listed_goal = open_goals
        .iter()
        .find(|g| g["id"].as_str() == Some(goal_id.as_str()))
        .unwrap_or_else(|| panic!("posted goal {goal_id} missing from open_goals: {}", listed.body));
    assert_eq!(
        listed_goal["title"].as_str(),
        Some("Ship the weather app"),
        "{}",
        listed.body
    );
    assert_eq!(
        listed_goal["status"].as_str(),
        Some("acting"),
        "{}",
        listed.body
    );
    assert_eq!(
        listed_goal["case_id"].as_str(),
        Some(case_id.as_str()),
        "{}",
        listed.body
    );

    let case = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/cases/{case_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(case.status, StatusCode::OK, "{}", case.body);
    assert_eq!(
        case.body["kind"].as_str(),
        Some("workforce"),
        "{}",
        case.body
    );
}

#[tokio::test]
async fn post_workforce_goal_blank_title_is_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let empty = call(
        &app,
        "POST",
        &workforce(&ws, "/goals"),
        Some(&jar),
        Some(json!({ "title": "" })),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);
    assert_eq!(
        empty.body["error"].as_str(),
        Some("title is required"),
        "{}",
        empty.body
    );

    let whitespace = call(
        &app,
        "POST",
        &workforce(&ws, "/goals"),
        Some(&jar),
        Some(json!({ "title": "   " })),
    )
    .await;
    assert_eq!(
        whitespace.status,
        StatusCode::BAD_REQUEST,
        "{}",
        whitespace.body
    );
    assert_eq!(
        whitespace.body["error"].as_str(),
        Some("title is required"),
        "{}",
        whitespace.body
    );

    let listed = call(&app, "GET", &workforce(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let open_goals = listed.body["open_goals"]
        .as_array()
        .unwrap_or_else(|| panic!("open_goals: {}", listed.body));
    assert!(
        open_goals.is_empty(),
        "blank title must not persist a goal: {}",
        listed.body
    );
}

#[tokio::test]
async fn cancel_workforce_goal_drops_open_goals() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &workforce(&ws, "/goals"),
        Some(&jar),
        Some(json!({ "title": "Ship the weather app" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let goal_id = created.body["goal"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("goal.id: {}", created.body))
        .to_string();

    let cancelled = call(
        &app,
        "POST",
        &workforce(&ws, &format!("/goals/{goal_id}/cancel")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(
        cancelled.body["goal"]["status"].as_str(),
        Some("cancelled"),
        "{}",
        cancelled.body
    );
    assert_eq!(
        cancelled.body["goal"]["id"].as_str(),
        Some(goal_id.as_str()),
        "{}",
        cancelled.body
    );

    let listed = call(&app, "GET", &workforce(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let open_goals = listed.body["open_goals"]
        .as_array()
        .unwrap_or_else(|| panic!("open_goals: {}", listed.body));
    assert!(
        open_goals.is_empty(),
        "cancelled goal must leave open_goals: {}",
        listed.body
    );
}

#[tokio::test]
async fn workforce_http_is_workspace_scoped() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let created = call(
        &app,
        "POST",
        &workforce(&a_ws, "/goals"),
        Some(&a_jar),
        Some(json!({ "title": "Ship the weather app" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let goal_id = created.body["goal"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("goal.id: {}", created.body))
        .to_string();

    let listed = call(&app, "GET", &workforce(&a_ws, ""), Some(&b_jar), None).await;
    assert_eq!(listed.status, StatusCode::FORBIDDEN, "{}", listed.body);

    let posted = call(
        &app,
        "POST",
        &workforce(&a_ws, "/goals"),
        Some(&b_jar),
        Some(json!({ "title": "foreign" })),
    )
    .await;
    assert_eq!(posted.status, StatusCode::FORBIDDEN, "{}", posted.body);

    let cancelled = call(
        &app,
        "POST",
        &workforce(&a_ws, &format!("/goals/{goal_id}/cancel")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::FORBIDDEN, "{}", cancelled.body);
}

#[tokio::test]
async fn session_live_comes_from_worker_ingest_not_plane_file() {
    use fleet_orchestrator::domain::{
        OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
    };
    use fleet_orchestrator::store::NewWorkerSession;

    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_id = Uuid::parse_str(&ws).expect("ws");
    let _ = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;

    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let body = WorkPlanBody {
        plan_id,
        project_id: ws_id,
        run_id,
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "test".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: "Build".into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "generalist".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: None,
            max_retries: None,
        }],
        policy: PlanPolicy {
            on_task_fail: OnTaskFailPolicy::FailRun,
            ..Default::default()
        },
    };
    state
        .orch
        .store
        .insert_accepted_plan(ws_id, body)
        .await
        .expect("plan");
    state
        .orch
        .store
        .materialize_plan(ws_id, plan_id)
        .await
        .expect("materialize");
    let snap = state
        .orch
        .store
        .load_board_snapshot(ws_id)
        .await
        .expect("snap");
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .expect("t1");
    assert!(
        state
            .orch
            .store
            .update_task_status(task.id, "queued", "ready", None)
            .await
            .expect("ready"),
        "t1 queued→ready"
    );
    let sid = Uuid::new_v4();
    assert!(state
        .orch
        .store
        .cas_assign_task(task.id, sid, 1)
        .await
        .expect("cas"));
    let token = "live-worker-token";
    state
        .orch
        .store
        .insert_session_spawning(&NewWorkerSession {
            id: sid,
            project_id: ws_id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "remote".into(),
            worker_token: token.into(),
            worker_profile: "generalist".into(),
            work_dir: Some("/this/plane/path/has/no/live.jsonl".into()),
        })
        .await
        .expect("session");

    let hb = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{sid}/heartbeat"),
        token,
        Some(json!({
            "seq": 3,
            "progress_pct": 40,
            "events": [{
                "kind": "update",
                "text": "hello from driver",
                "ts": "2026-09-02T17:00:00Z"
            }]
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::ACCEPTED, "{}", hb.body);

    let again = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{sid}/heartbeat"),
        token,
        Some(json!({
            "seq": 3,
            "progress_pct": 40,
            "events": [{ "kind": "update", "text": "duplicate seq must not land" }]
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.body);

    let detail = call(
        &app,
        "GET",
        &fleet(&ws, &format!("/sessions/{sid}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    let live = detail.body["live"].as_array().cloned().unwrap_or_default();
    assert_eq!(live.len(), 1, "live: {}", detail.body);
    assert_eq!(live[0]["text"], "hello from driver");
}

#[tokio::test]
async fn planner_goal_creates_accepted_plan_job() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/plans"),
        Some(&jar),
        Some(json!({ "goal": "Ship the weather app", "worker_profile": "coding" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["status"], "accepted");
    assert!(created.body["run_id"].as_str().is_some(), "{}", created.body);
    let job_id = created.body["id"].as_str().expect("job id");
    let got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/plans/{job_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "accepted");
}

#[tokio::test]
async fn planner_foreign_job_on_own_workspace_is_404() {
    let (app, _state) = app_with_state().await;
    let (_a, jar_a, ws_a) = signup_and_login(&app).await;
    let (_b, jar_b, ws_b) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/plans"),
        Some(&jar_a),
        Some(json!({ "goal": "secret plan" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let job_id = created.body["id"].as_str().expect("job id");
    let got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_b}/plans/{job_id}"),
        Some(&jar_b),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::NOT_FOUND, "{}", got.body);
}

#[tokio::test]
async fn fleet_cloud_attach_creates_sandboxed_host() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/runners/cloud/fleet"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let host = &created.body["host"];
    assert_eq!(host["isolation"], "sandboxed", "{}", created.body);
    assert_eq!(host["runner_kind"], "fleet_cloud", "{}", created.body);
    assert!(created.body["pair_token"].as_str().is_some(), "{}", created.body);
}

#[tokio::test]
async fn byo_cloud_requires_credentials_and_stays_sandboxed() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let missing = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/runners/cloud/byo"),
        Some(&jar),
        Some(json!({ "provider": "aws" })),
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST, "{}", missing.body);
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/runners/cloud/byo"),
        Some(&jar),
        Some(json!({
            "provider": "aws",
            "credentials": { "access_key_id": "AKIAFAKE", "secret": "x" }
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["host"]["isolation"], "sandboxed");
    assert_eq!(created.body["host"]["runner_kind"], "public_cloud");
    assert!(
        created.body.to_string().to_ascii_lowercase().contains("akiafake") == false,
        "credentials must not echo: {}",
        created.body
    );
}
