//! Fail-closed HTTP integration tests for fleet-server (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Once;
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

fn set_env(key: &str, val: &str) {
    // Safety: tests set process env once before any config/app is built.
    unsafe { std::env::set_var(key, val) };
}

fn install_test_env() {
    static ONCE: Once = std::sync::Once::new();
    ONCE.call_once(|| {
        set_env("SANDBOX_BACKEND", "stub");
        set_env("STUB_DURATION_S", "0");
        set_env("COOKIE_SECURE", "false");
        set_env("ADMIN_TOKEN", TEST_ADMIN_TOKEN);
        let jwt_ok = std::env::var("JWT_SECRET")
            .ok()
            .filter(|s| s.len() >= 32)
            .is_some();
        if !jwt_ok {
            set_env("JWT_SECRET", TEST_JWT_SECRET);
        }
    });
}

fn test_integrations_config(jwt_secret: &str) -> IntegrationsConfig {
    IntegrationsConfig {
        api_public_base: "http://localhost:8080".into(),
        ui_public_base: "http://localhost:3030".into(),
        state_secret: jwt_secret.to_string(),
        credentials_key: CredentialsKey::from_secret(jwt_secret).expect("credentials key"),
        github: None,
        slack: None,
        linear: None,
        jira: None,
        figma: None,
        notion: None,
        google_drive: None,
        teams: None,
        discord: None,
    }
}

async fn app() -> axum::Router {
    install_test_env();
    let database_url = require_database_url();

    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);

    let orch_cfg = OrchConfig {
        database_url: database_url.clone(),
        admin_token: TEST_ADMIN_TOKEN.into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
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

    let auth = AuthConfig::from_env().expect("AuthConfig::from_env");
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
    };

    build_http_app(state, orch, &config).expect("build_http_app")
}

#[derive(Debug, Clone)]
struct ParsedSetCookie {
    name: String,
    value: String,
    #[allow(dead_code)]
    raw: String,
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

// ── 3. fixture chain + tick until done ──────────────────────────────────────

#[tokio::test]
async fn fixture_chain_ticks_until_t1_t2_done_success() {
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
    assert!(fx.body["run_id"].as_str().is_some(), "{}", fx.body);

    let tasks = tick_until_tasks_done(&app, &jar, &ws, &["t1", "t2"]).await;
    let list = tasks["tasks"].as_array().unwrap();
    for id in ["t1", "t2"] {
        let t = list
            .iter()
            .find(|t| t["plan_task_id"] == id)
            .unwrap_or_else(|| panic!("missing {id} in {tasks}"));
        assert_eq!(t["status"], "done_success", "{id}: {tasks}");
    }
}

// ── 4. fixture diamond ──────────────────────────────────────────────────────

#[tokio::test]
async fn fixture_diamond_materializes_root_left_right_join() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "diamond" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "{}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let ids: Vec<String> = tasks.body["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t["plan_task_id"].as_str().map(str::to_string))
        .collect();
    for want in ["root", "left", "right", "join"] {
        assert!(
            ids.iter().any(|id| id == want),
            "plan_task_ids missing {want}: {ids:?} body={}",
            tasks.body
        );
    }
}

// ── 5. cancel run ───────────────────────────────────────────────────────────

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

// ── 6. PATCH fleet config ───────────────────────────────────────────────────

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

// ── 7. workers / sessions after stub run ────────────────────────────────────

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

// ── 8. personas ─────────────────────────────────────────────────────────────

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
}

// ── 9. integrations + oauth ─────────────────────────────────────────────────

#[tokio::test]
async fn integrations_github_available_gitlab_coming_soon_oauth_callback_no_token() {
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
    let gitlab = items
        .iter()
        .find(|i| i["id"] == "gitlab")
        .unwrap_or_else(|| panic!("gitlab missing: {}", list.body));
    assert_eq!(github["availability"], "available", "{github}");
    assert_eq!(gitlab["availability"], "coming_soon", "{gitlab}");

    let start = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations/gitlab/oauth/start"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(start.status, StatusCode::BAD_REQUEST, "{}", start.body);

    let cb = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/github",
        None,
        None,
    )
    .await;
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
}

// ── 10. chat conversations ──────────────────────────────────────────────────

#[tokio::test]
async fn chat_status_create_list_delete_other_user_forbidden() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let chat = format!("/api/workspaces/{ws}/chat");

    let status = call(&app, "GET", &format!("{chat}/status"), Some(&jar), None).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);

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
