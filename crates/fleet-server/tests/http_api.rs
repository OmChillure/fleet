//! Fail-closed HTTP integration tests for fleet-server (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use fleet_auth::{AuthConfig, ACCESS_COOKIE, REFRESH_COOKIE};
use fleet_db::{connect_and_migrate_with, PoolConfig};
use fleet_integrations::{CredentialsKey, IntegrationsConfig};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{build_http_app, AppState, GrokConfig, OllamaConfig, ServerConfig};
use http_body_util::BodyExt;
use serde_json::{json, Value};
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
        sandbox_backend: "stub".into(),
        stub_duration: Duration::from_millis(0),
        process_worker_argv: vec!["fleet-worker".into()],
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
        worker_dist_dir: config.worker_dist_dir.clone(),
    };

    let router = build_http_app(state.clone(), orch, &config).expect("build_http_app");
    (router, state)
}

async fn app() -> axum::Router {
    app_with_state().await.0
}

/// JWT shaped like OAuth state but with `typ: "access"` (must be rejected by callback).
fn oauth_state_with_typ_access(secret: &str, provider: &str) -> String {
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;

    #[derive(Serialize)]
    struct AccessShaped {
        sub: String,
        workspace_id: String,
        provider: String,
        nonce: String,
        exp: i64,
        iat: i64,
        typ: String,
    }

    let now = chrono::Utc::now().timestamp();
    encode(
        &Header::default(),
        &AccessShaped {
            sub: Uuid::new_v4().to_string(),
            workspace_id: Uuid::new_v4().to_string(),
            provider: provider.to_string(),
            nonce: Uuid::now_v7().to_string(),
            exp: now + 600,
            iat: now,
            typ: "access".into(),
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("encode access-typed oauth state")
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

fn partners_disclaimer(ws: &str) -> String {
    format!("/api/workspaces/{ws}/partners/disclaimer")
}

async fn tick_until_tasks_done(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    want: &[&str],
) -> Value {
    let mut last_tasks = json!({});
    for i in 0..24 {
        let tick = call(app, "POST", &fleet(ws, "/tick"), Some(jar), None).await;
        assert_eq!(tick.status, StatusCode::OK, "tick {i}: {}", tick.body);
        tokio::time::sleep(Duration::from_millis(75)).await;
        let tasks = call(app, "GET", &fleet(ws, "/tasks"), Some(jar), None).await;
        assert_eq!(tasks.status, StatusCode::OK, "tasks {i}: {}", tasks.body);
        last_tasks = tasks.body;
        let list = last_tasks["tasks"].as_array().cloned().unwrap_or_default();
        let done = want.iter().all(|id| {
            list.iter().any(|t| {
                t["plan_task_id"].as_str() == Some(*id) && t["status"] == "done_success"
            })
        });
        if done && !list.is_empty() {
            return last_tasks;
        }
    }
    panic!("tasks did not reach done_success for {want:?} within 24 ticks: {last_tasks}");
}

// ── 1. health / healthz / api index ─────────────────────────────────────────

#[tokio::test]
async fn health_ok_healthz_ok_api_index_lists_auth_and_fleet() {
    let app = app().await;

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK, "{}", health.body);
    assert_eq!(health.body["status"], "ok");
    assert_eq!(health.body["service"], "fleet-server");
    assert_eq!(health.body["db"], "up");
    let apis = health.body["apis"]
        .as_array()
        .expect("health.apis")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    for need in ["auth", "workspaces", "personas", "integrations", "chat", "fleet"] {
        assert!(
            apis.contains(&need),
            "health.apis must advertise {need}: {apis:?}"
        );
    }
    assert!(
        health.body.get("git_sha").is_some(),
        "health must expose git_sha (null when FLEET_GIT_SHA unset at compile): {}",
        health.body
    );

    let healthz = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(healthz.status, StatusCode::OK, "{}", healthz.body);
    assert_eq!(healthz.body["status"], "ok");

    let api = call(&app, "GET", "/api", None, None).await;
    assert_eq!(api.status, StatusCode::OK, "{}", api.body);
    assert!(
        api.body["apis"]["auth"].is_object(),
        "api index must list auth: {}",
        api.body
    );
    assert!(
        api.body["apis"]["fleet"].is_object(),
        "api index must list fleet: {}",
        api.body
    );
    assert!(
        api.body["apis"]["chat"].is_object(),
        "api index must list chat: {}",
        api.body
    );
    assert!(
        api.body["apis"]["personas"].is_object(),
        "api index must list personas: {}",
        api.body
    );
    assert!(
        api.body["apis"]["integrations"].is_object(),
        "api index must list integrations: {}",
        api.body
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

// ── 3. one BFF fixture + tick with cookie (orch e2e lives in fleet-orchestrator) ─

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
    assert!(fx.body["run_id"].as_str().is_some(), "{}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let list = tasks.body["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        !list.is_empty(),
        "fixture + tick must materialize tasks: {}",
        tasks.body
    );
    assert!(
        list.iter().any(|t| t["plan_task_id"].as_str() == Some("t1")),
        "expected plan_task_id t1: {}",
        tasks.body
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
    assert_eq!(cancel.body["cancel_requested"], true);

    let mut last = json!({});
    for i in 0..24 {
        let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
        assert_eq!(tick.status, StatusCode::OK, "tick {i}: {}", tick.body);
        tokio::time::sleep(Duration::from_millis(75)).await;
        let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
        assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
        last = runs.body.clone();
        let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
        let run = run_list
            .iter()
            .find(|r| r["id"].as_str() == Some(run_id.as_str()));
        if run.map(|r| r["status"] == "cancelled").unwrap_or(false) {
            return;
        }
    }
    panic!("run {run_id} did not become cancelled within 24 ticks: {last}");
}

// ── 5. PATCH fleet config ───────────────────────────────────────────────────

#[tokio::test]
async fn patch_fleet_config_round_trip_empty_clears_git_ref_only_400() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let primed = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(primed.status, StatusCode::OK, "{}", primed.body);

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
        "https://github.com/acme/app.git"
    );
    assert_eq!(patched.body["config"]["git_ref"], "develop");

    let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["config"]["repo_url"],
        "https://github.com/acme/app.git"
    );
    assert_eq!(got.body["config"]["git_ref"], "develop");

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

    let ref_only = call(
        &app,
        "PATCH",
        &fleet(&ws, "/config"),
        Some(&jar),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(ref_only.status, StatusCode::BAD_REQUEST, "{}", ref_only.body);
}

// ── 6. workers / sessions after stub run ────────────────────────────────────

#[tokio::test]
async fn workers_sandboxes_match_sessions_and_detail_shape() {
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
    let _ = tick_until_tasks_done(&app, &jar, &ws, &["t1", "t2"]).await;

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let session_ids: Vec<String> = sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        !session_ids.is_empty(),
        "expected sessions after stub run: {}",
        sessions.body
    );

    let workers = call(&app, "GET", &fleet(&ws, "/workers"), Some(&jar), None).await;
    assert_eq!(workers.status, StatusCode::OK, "{}", workers.body);
    let sandbox_ids: Vec<String> = workers.body["sandboxes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        sandbox_ids, session_ids,
        "workers.sandboxes ids must match sessions ids\nworkers={}\nsessions={}",
        workers.body, sessions.body
    );

    let sid = &session_ids[0];
    let detail = call(
        &app,
        "GET",
        &fleet(&ws, &format!("/sessions/{sid}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert!(
        detail.body["session"].is_object(),
        "session: {}",
        detail.body
    );
    assert!(
        detail.body.get("prompt").is_some(),
        "prompt: {}",
        detail.body
    );
    assert!(detail.body.get("live").is_some(), "live: {}", detail.body);
    assert!(
        detail.body.get("transcript").is_some(),
        "transcript: {}",
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
}

// ── 7. personas ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn personas_seed_starters_create_patch_archive() {
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

    let second = call(
        &app,
        "POST",
        &format!("{base}/seed-starters"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_eq!(second.body["inserted"], 0, "{}", second.body);
    assert_eq!(second.body["skipped"], 8, "{}", second.body);

    let created = call(
        &app,
        "POST",
        &base,
        Some(&jar),
        Some(json!({
            "name": "Custom reviewer",
            "slug": format!("custom-reviewer-{}", &Uuid::new_v4().to_string()[..8]),
            "description": "Reviews PRs",
            "role_prompt": "Be thorough.",
            "worker_profile": "coding"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("persona id: {}", created.body))
        .to_string();

    let patched = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({ "description": "Updated reviewer", "name": "Reviewer v2" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["name"], "Reviewer v2");
    assert_eq!(patched.body["description"], "Updated reviewer");

    let archived = call(&app, "DELETE", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(
        archived.status,
        StatusCode::NO_CONTENT,
        "{}",
        archived.body
    );

    let got = call(&app, "GET", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["status"], "archived");

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["personas"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(
        !ids.contains(&pid.as_str()),
        "default persona list is active-only; archived {pid} leaked: {ids:?}"
    );
}

// ── 8. integrations + oauth ─────────────────────────────────────────────────

#[tokio::test]
async fn integrations_catalog_oauth_start_delete_and_callback_contracts() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    // Catalog: github + gitlab available; bitbucket still coming_soon.
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
    let gitlab = items
        .iter()
        .find(|i| i["id"] == "gitlab")
        .unwrap_or_else(|| panic!("gitlab missing: {}", list.body));
    let bitbucket = items
        .iter()
        .find(|i| i["id"] == "bitbucket")
        .unwrap_or_else(|| panic!("bitbucket missing: {}", list.body));
    let trello = items
        .iter()
        .find(|i| i["id"] == "trello")
        .unwrap_or_else(|| panic!("trello missing: {}", list.body));
    let asana = items
        .iter()
        .find(|i| i["id"] == "asana")
        .unwrap_or_else(|| panic!("asana missing: {}", list.body));
    assert_eq!(github["availability"], "available", "{github}");
    assert_eq!(gitlab["availability"], "available", "{gitlab}");
    assert_eq!(trello["availability"], "available", "{trello}");
    assert_eq!(asana["availability"], "available", "{asana}");
    assert_eq!(bitbucket["availability"], "coming_soon", "{bitbucket}");
    assert_eq!(
        github["oauth_start_path"],
        format!("/api/workspaces/{ws}/integrations/github/oauth/start"),
        "available providers always expose oauth/start (app can bootstrap): {github}"
    );
    assert_eq!(
        gitlab["oauth_start_path"],
        format!("/api/workspaces/{ws}/integrations/gitlab/oauth/start"),
        "available providers always expose oauth/start (app can bootstrap): {gitlab}"
    );
    assert_eq!(
        bitbucket["oauth_start_path"],
        serde_json::Value::Null,
        "coming-soon providers must not expose oauth/start: {bitbucket}"
    );

    // Coming-soon OAuth start → 4xx (not 500 / not 302).
    let start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/bitbucket/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert!(
        start.status.is_client_error(),
        "coming-soon oauth start must be 4xx not {}: {}",
        start.status,
        start.body
    );
    assert_eq!(start.status, StatusCode::BAD_REQUEST, "{}", start.body);
    assert!(
        start.body["error"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("not available"),
        "coming-soon start should say not available: {}",
        start.body
    );

    // GitLab without a server client serves the one-time OAuth app form.
    let gl_start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/gitlab/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        gl_start.status,
        StatusCode::OK,
        "GitLab without a server client must serve the bootstrap form, not 4xx/302: {} {}",
        gl_start.status,
        gl_start.raw
    );
    let gl_ct = gl_start
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        gl_ct.contains("text/html"),
        "expected HTML bootstrap form, content-type={gl_ct:?} body={}",
        gl_start.raw
    );
    assert!(
        gl_start.raw.contains("client_id") && gl_start.raw.contains("client_secret"),
        "GitLab start should ask for an OAuth app: {}",
        gl_start.raw
    );

    // Trello authorize lands on a fragment-token capture page when no code is present.
    let tr_cb = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/trello",
        None,
        None,
    )
    .await;
    assert_eq!(tr_cb.status, StatusCode::OK, "{}", tr_cb.raw);
    assert!(
        tr_cb.raw.contains("location.hash") && tr_cb.raw.contains("token"),
        "Trello callback without code should lift #token into ?code=: {}",
        tr_cb.raw
    );

    let gh_start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/github/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        gh_start.status,
        StatusCode::OK,
        "GitHub without a server client must serve the App Manifest form, not 4xx/302: {} {}",
        gh_start.status,
        gh_start.raw
    );
    let ct = gh_start
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/html"),
        "expected HTML manifest form, content-type={ct:?} body={}",
        gh_start.raw
    );
    assert!(
        gh_start.raw.contains("github.com/settings/apps/new")
            && gh_start.raw.contains("name=\"manifest\""),
        "GitHub start should POST an App Manifest to github.com: {}",
        gh_start.raw
    );

    let unknown_start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/notaprovider/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        unknown_start.status,
        StatusCode::NOT_FOUND,
        "{}",
        unknown_start.body
    );

    // DELETE: unknown provider and not-connected known provider are both 404
    // (routes.rs: is_known_provider → not_found; disconnect false → not_found).
    let disc_unknown = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/integrations/notaprovider"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        disc_unknown.status,
        StatusCode::NOT_FOUND,
        "unknown provider disconnect: {}",
        disc_unknown.body
    );

    let disc = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/integrations/github"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        disc.status,
        StatusCode::NOT_FOUND,
        "not-connected github disconnect: {}",
        disc.body
    );
    assert!(
        disc.body["error"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("not connected")
            || disc.body["error"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("github"),
        "expected not-connected message: {}",
        disc.body
    );

    // Callback without code/state: HTML close page, no token leak, not 5xx.
    let cb = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/github",
        None,
        None,
    )
    .await;
    assert!(
        !cb.status.is_server_error(),
        "oauth callback must not 5xx: {} {}",
        cb.status,
        cb.raw
    );
    assert!(
        cb.status == StatusCode::OK || cb.status == StatusCode::BAD_REQUEST,
        "oauth callback status {}: {}",
        cb.status,
        cb.raw
    );
    assert!(
        cb.raw.to_ascii_lowercase().contains("<html") || cb.body.get("raw").is_some(),
        "expected HTML callback body: {}",
        cb.raw
    );
    assert!(
        !cb.raw.contains("access_token"),
        "callback must not leak access_token: {}",
        cb.raw
    );

    // Callback with typ=access state JWT must be rejected (same secret as state_secret).
    // Mirrors fleet_integrations oauth state_token unit; exercises the HTTP path.
    let bad_state = oauth_state_with_typ_access(TEST_JWT_SECRET, "github");
    let cb_access = call(
        &app,
        "GET",
        &format!(
            "/api/integrations/oauth/callback/github?code=fake-code&state={bad_state}"
        ),
        None,
        None,
    )
    .await;
    assert!(
        !cb_access.status.is_server_error(),
        "typ=access state must not 5xx: {} {}",
        cb_access.status,
        cb_access.raw
    );
    assert_eq!(
        cb_access.status,
        StatusCode::OK,
        "callback always returns HTML close page: {}",
        cb_access.raw
    );
    let lower = cb_access.raw.to_ascii_lowercase();
    assert!(
        lower.contains("connection failed")
            || lower.contains("invalid")
            || lower.contains("failed"),
        "access-typed state must fail connection: {}",
        cb_access.raw
    );
    assert!(
        lower.contains("ok: false") || lower.contains("ok:false"),
        "popup payload must report ok=false: {}",
        cb_access.raw
    );
    assert!(
        !cb_access.raw.contains("access_token"),
        "must not leak access_token: {}",
        cb_access.raw
    );
}

// ── 9. chat conversations ───────────────────────────────────────────────────

#[tokio::test]
async fn chat_status_create_list_delete_other_user_forbidden() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let chat = format!("/api/workspaces/{ws}/chat");

    let status = call(&app, "GET", &format!("{chat}/status"), Some(&jar), None).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    let providers = status.body["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect::<Vec<_>>();
    assert!(providers.contains(&"grok"), "{providers:?}");
    assert!(providers.contains(&"ollama"), "{providers:?}");
    let openai = status.body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "openai");
    if let Some(p) = openai {
        assert_eq!(p["coming_soon"], true, "{p}");
        assert_eq!(p["available"], false, "{p}");
    }

    let empty = call(
        &app,
        "POST",
        &chat,
        Some(&jar),
        Some(json!({ "messages": [], "provider": "ollama" })),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);

    let unknown_provider = call(
        &app,
        "POST",
        &chat,
        Some(&jar),
        Some(json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "openai",
            "model": "gpt-4"
        })),
    )
    .await;
    assert_eq!(
        unknown_provider.status,
        StatusCode::BAD_REQUEST,
        "unknown provider must not 5xx: {}",
        unknown_provider.body
    );

    let grok_off = call(
        &app,
        "POST",
        &chat,
        Some(&jar),
        Some(json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "grok"
        })),
    )
    .await;
    assert!(
        grok_off.status.is_client_error(),
        "disabled grok must be a client error, not {}: {}",
        grok_off.status,
        grok_off.body
    );

    let created = call(
        &app,
        "POST",
        &format!("{chat}/conversations"),
        Some(&jar),
        Some(json!({ "title": "http_api chat" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let cid = created.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created.body))
        .to_string();

    let listed = call(
        &app,
        "GET",
        &format!("{chat}/conversations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let convs = listed.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", listed.body));
    assert!(
        convs.iter().any(|c| c["id"].as_str() == Some(cid.as_str())),
        "list missing {cid}: {}",
        listed.body
    );

    let got = call(
        &app,
        "GET",
        &format!("{chat}/conversations/{cid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);

    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let other = call(
        &app,
        "GET",
        &format!("{chat}/conversations/{cid}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert!(
        other.status == StatusCode::FORBIDDEN || other.status == StatusCode::NOT_FOUND,
        "other user must not read conversation: {} {}",
        other.status,
        other.body
    );

    let deleted = call(
        &app,
        "DELETE",
        &format!("{chat}/conversations/{cid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body["deleted"], true);
}

// ── 10. chat tools without live LLM (direct execute_tool + stub sandbox) ────

#[tokio::test]
async fn chat_tool_launch_agent_task_builds_plan_and_settles_under_stub() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");

    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "http_api stub launch",
            "spec": "Settle under stub sandbox without a live LLM.",
            "worker_profile": "coding"
        }),
        &fleet_server::chat_tools::ChatLlmContext {
            provider: "grok".into(),
            model: "grok-4.5".into(),
        },
    )
    .await;

    assert!(
        action.ok,
        "launch_agent_task should succeed under stub: action={action:?} content={content}"
    );
    assert_eq!(action.tool, "launch_agent_task");
    assert!(
        action.run_id.as_deref().filter(|s| !s.is_empty()).is_some(),
        "run_id: {action:?}"
    );
    assert_eq!(
        content["run_terminal"], true,
        "stub ticks should settle the single-task run: {content}"
    );
    assert_eq!(content["task_count"], 1, "{content}");
    let tasks = content["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks array: {content}"));
    let t1 = tasks
        .iter()
        .find(|t| t["plan_task_id"] == "t1")
        .unwrap_or_else(|| panic!("missing t1: {content}"));
    assert_eq!(
        t1["status"], "done_success",
        "chat plan t1 should finish under stub: {content}"
    );

    // Same workspace cookie BFF should see the run the tool materialized.
    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "{}", runs.body);
    let run_id = action.run_id.as_deref().unwrap();
    let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
    assert!(
        run_list
            .iter()
            .any(|r| r["id"].as_str() == Some(run_id)),
        "BFF runs missing tool run_id {run_id}: {}",
        runs.body
    );
}

// ── 11. /v1 admin bearer vs cookie ──────────────────────────────────────────

#[tokio::test]
async fn v1_projects_rejects_missing_bearer_and_session_cookie() {
    let app = app().await;
    let (_email, jar, _ws) = signup_and_login(&app).await;
    let body = json!({ "name": format!("v1-{}", Uuid::new_v4()), "max_agents": 2 });

    let no_auth = call(&app, "POST", "/v1/projects", None, Some(body.clone())).await;
    assert_eq!(no_auth.status, StatusCode::UNAUTHORIZED, "{}", no_auth.body);

    let cookie_only = call(&app, "POST", "/v1/projects", Some(&jar), Some(body.clone())).await;
    assert_eq!(
        cookie_only.status,
        StatusCode::UNAUTHORIZED,
        "cookie must not authorize /v1: {}",
        cookie_only.body
    );

    // ADMIN_TOKEN is configured; bearer must work so the 401s above are not
    // "admin automation disabled".
    let with_admin = call_inner(
        &app,
        "POST",
        "/v1/projects",
        None,
        Some(TEST_ADMIN_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(
        with_admin.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN bearer should create a project: {}",
        with_admin.body
    );
}

#[tokio::test]
async fn fleet_requires_membership() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;

    let anon = call(&app, "GET", &fleet(&a_ws, ""), None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{}", anon.body);

    let other = call(&app, "GET", &fleet(&a_ws, ""), Some(&b_jar), None).await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.body);

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let _ = tick_until_tasks_done(&app, &a_jar, &a_ws, &["t1"]).await;
    let sessions = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    let sid = sessions.body["sessions"][0]["id"].as_str().expect("session");
    let stolen = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::FORBIDDEN,
        "other member must not read session detail: {}",
        stolen.body
    );
}

// ── 12. leftover: fleet settings + agency-agents personas ───────────────────

#[tokio::test]
async fn fleet_settings_get_patch_and_rejects_grok_store() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let got = call(&app, "GET", &fleet(&ws, "/settings"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["fleet_llm"], "off", "{}", got.body);
    assert_eq!(got.body["grok_env"], false, "unit/http tests force grok env off");
    assert!(
        got.body["max_agents_cap"].as_i64().unwrap_or(0) >= 16,
        "{}",
        got.body
    );

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
    assert_eq!(patched.body["fleet_llm_model"], "llama3.2", "{}", patched.body);
    assert_eq!(patched.body["effective_backend"], "ollama", "{}", patched.body);

    let grok = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({ "fleet_llm": "grok" })),
    )
    .await;
    assert_eq!(grok.status, StatusCode::BAD_REQUEST, "{}", grok.body);

    let project = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(project.status, StatusCode::OK, "{}", project.body);
    assert_eq!(
        project.body["config"]["fleet_llm"], "ollama",
        "GET /fleet should include settings: {}",
        project.body
    );
}

#[tokio::test]
async fn fleet_personas_lists_builtin_or_roster() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let out = call(&app, "GET", &fleet(&ws, "/personas"), Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    let personas = out.body["personas"]
        .as_array()
        .unwrap_or_else(|| panic!("personas array: {}", out.body));
    assert!(
        !personas.is_empty(),
        "expected builtin or cloned roster: {}",
        out.body
    );
    assert!(
        personas.iter().any(|p| p["slug"]
            .as_str()
            .unwrap_or("")
            .contains("backend")
            || p["slug"].as_str().unwrap_or("").contains("reviewer")
            || p["name"].as_str().is_some()),
        "{}",
        out.body
    );
}

#[tokio::test]
async fn chat_tool_list_agent_personas_and_launch_plan_stamps_llm() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5-internal".into(),
    };

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_agent_personas",
        &json!({}),
        &llm,
    )
    .await;
    assert!(action.ok, "{action:?} {listed}");
    assert_eq!(action.tool, "list_agent_personas");
    assert!(
        listed["count"].as_u64().unwrap_or(0) >= 1,
        "{listed}"
    );

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
        &llm,
    )
    .await;
    assert!(launched.ok, "{launched:?} {content}");
    assert_eq!(content["task_count"], 2, "{content}");
    let run_id = launched.run_id.as_deref().expect("run_id");
    let run_uuid = Uuid::parse_str(run_id).unwrap();
    let snap = state
        .orch
        .store
        .load_board_snapshot(workspace_id)
        .await
        .unwrap();
    assert_eq!(
        snap.tasks.iter().filter(|t| t.run_id == run_uuid).count(),
        2
    );
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
async fn chat_tools_human_can_talk_on_team_channel() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

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
        &llm,
    )
    .await;
    assert!(launched.ok, "{launched:?} {content}");
    let run_id = launched.run_id.as_deref().expect("run_id");

    let (sent, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": run_id, "to": "t1", "body": "use port 9090" }),
        &llm,
    )
    .await;
    assert!(action.ok, "{action:?} {sent}");
    assert_eq!(sent["to"], "t1", "{sent}");
    assert_eq!(sent["from"], "human", "{sent}");

    let (read, read_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "read_team_chat",
        &json!({ "run_id": run_id }),
        &llm,
    )
    .await;
    assert!(read_action.ok, "{read_action:?} {read}");
    assert_eq!(read["count"], 1, "{read}");
    assert!(
        read["transcript"]
            .as_str()
            .unwrap_or("")
            .contains("use port 9090"),
        "{read}"
    );

    let overview = call(
        &app,
        "GET",
        &fleet(&ws, "/overview"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    assert!(
        overview.body["total_runs"].as_u64().unwrap_or(0) >= 1,
        "{}",
        overview.body
    );
    assert_eq!(overview.body["chat"].as_array().map(|a| a.len()), Some(1));
}

#[tokio::test]
async fn chat_tools_monitor_needs_a_board_then_finish_launches_from_inbox() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let workspace_id = Uuid::parse_str(&ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let (denied, deny_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "monitor_tickets",
        &json!({}),
        &llm,
    )
    .await;
    assert!(!deny_action.ok, "{deny_action:?} {denied}");
    let err = denied["error"].as_str().unwrap_or("");
    assert!(
        err.contains("Connect") || err.contains("connected"),
        "{denied}"
    );

    fleet_db::repos::tickets::upsert_inbox(
        &state.db,
        &fleet_db::repos::tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-42".into(),
            title: "Ship the inbox".into(),
            url: Some("https://linear.app/eng-42".into()),
            summary: "Do the thing described on the ticket.".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("seed inbox");

    let listed = call(
        &app,
        "GET",
        &fleet(&ws, "/tickets"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["count"], 1, "{}", listed.body);
    assert_eq!(listed.body["tickets"][0]["key"], "ENG-42");

    let prepared = fleet_server::tickets::prepare_finish(
        &state,
        workspace_id,
        user_id,
        fleet_server::tickets::FinishTicketArgs {
            ticket: Some("ENG-42".into()),
            provider: None,
            key: None,
        },
    )
    .await
    .expect("prepare");
    assert_eq!(prepared.item.external_key, "ENG-42");
    assert_eq!(prepared.tasks.len(), 2);
    assert_eq!(
        prepared.tasks[0].external_ref.as_ref().map(|e| e.key.as_str()),
        Some("ENG-42")
    );
    assert_eq!(prepared.tasks[1].worker_profile, "reviewer");

    let run_id = Uuid::now_v7();
    fleet_server::tickets::mark_ticket_launched(
        &state,
        workspace_id,
        "linear",
        "ENG-42",
        run_id,
    )
    .await
    .expect("mark launched");

    let again = call(
        &app,
        "GET",
        &fleet(&ws, "/tickets"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(again.body["tickets"][0]["state"], "launched", "{}", again.body);
    assert_eq!(
        again.body["tickets"][0]["run_id"].as_str(),
        Some(run_id.to_string().as_str()),
        "{}",
        again.body
    );

    let overview = call(
        &app,
        "GET",
        &fleet(&ws, "/overview"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    let tickets = overview.body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", overview.body));
    assert!(
        tickets.iter().any(|t| t["key"] == "ENG-42"),
        "{}",
        overview.body
    );

    let (replay, replay_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "finish_ticket",
        &json!({ "ticket": "ENG-42" }),
        &llm,
    )
    .await;
    assert!(replay_action.ok, "{replay_action:?} {replay}");
    assert_eq!(replay["already_launched"], true, "{replay}");
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
    assert!(
        posted.body["accepted_at"].as_str().is_some(),
        "ack is a timestamp only: {}",
        posted.body
    );

    let hosts = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/worker-hosts"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(hosts.status, StatusCode::OK, "hosts: {}", hosts.body);
    let host_list = hosts.body["hosts"].as_array().cloned().unwrap_or_default();
    assert!(
        host_list.is_empty(),
        "ack must not create worker_hosts: {}",
        hosts.body
    );

    let listed = fleet_db::repos::worker_hosts::list_for_workspace(&state.db, workspace_id)
        .await
        .expect("list_for_workspace");
    assert!(
        listed.is_empty(),
        "ack must not insert worker_hosts rows: {listed:?}"
    );

    let assignable = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(30),
    )
    .await
    .expect("list_assignable");
    assert!(
        assignable.is_empty(),
        "ack must not create assignable hosts: {assignable:?}"
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

    let claimed = fleet_db::repos::work_claims::claim_next(
        &state.db,
        workspace_id,
        Uuid::now_v7(),
        chrono::Duration::minutes(1),
    )
    .await
    .expect("claim_next");
    assert!(
        claimed.is_none(),
        "ack must not enqueue work_claims: {claimed:?}"
    );
}
