//! Host-mode e2e: pair/lease, job-token /v1 inbox, session healthy.
//!
//! Oneshot only (no live bind, no `fleet-host` child). Requires DATABASE_URL
//! — never skips.

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

/// In-process app + state (copied from worker_hosts_http.rs; do not import private items).
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
        // Non-loopback so spawn does not refuse remote assign (off-box
        // hosts cannot heartbeat to 127.0.0.1). In-process tests do not
        // dial this URL.
        orchestrator_public_url: "http://172.18.0.1:0".into(),
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

async fn signup_and_login(app: &axum::Router) -> (String, CookieJar, String) {
    let email = unique_email();
    let created = signup(app, &email, TEST_PASSWORD).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "signup: {}",
        created.raw
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

fn fleet(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/fleet")
    } else {
        format!("/api/workspaces/{ws}/fleet{suffix}")
    }
}

/// Pair + machine heartbeat so the host is online with `advertised_host=true`.
/// Process-only advertise cannot take the workspace default `container` pin.
async fn create_pair_and_heartbeat(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    name: &str,
) -> (Uuid, String) {
    let iso = call(
        app,
        "PATCH",
        &fleet(ws, "/settings"),
        Some(jar),
        Some(json!({ "min_isolation": "process" })),
    )
    .await;
    assert_eq!(
        iso.status,
        StatusCode::OK,
        "min_isolation=process for process-only host: {}",
        iso.body
    );
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
            "advertised_host": true,
            "advertised_container": false,
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
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 2
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);
    assert_eq!(hb.body["host"]["status"], "online", "{}", hb.body);
    assert_eq!(hb.body["effective"]["host"], true, "{}", hb.body);
    (host_id, machine)
}

/// Materialize t1 as a spawning remote session with a known job token.
/// No pair/lease/claim — those are locked by `tick_assigns_remote_host_instead_of_local_stub`.
async fn plant_spawning_remote_session(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
) -> (Uuid, String) {
    let fleet_proj = call(app, "GET", &fleet(ws, ""), Some(jar), None).await;
    assert_eq!(
        fleet_proj.status,
        StatusCode::OK,
        "ensure fleet project: {}",
        fleet_proj.body
    );

    let fx = call(
        app,
        "POST",
        &fleet(ws, "/runs/fixture"),
        Some(jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);
    let plan_id =
        Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan uuid");
    let workspace_id = Uuid::parse_str(ws).expect("ws");

    let n = state
        .orch
        .store
        .materialize_plan(workspace_id, plan_id)
        .await
        .expect("materialize_plan");
    assert!(n >= 1, "fixture must materialize at least t1");

    let tasks = state
        .orch
        .store
        .list_tasks_slim(workspace_id, None)
        .await
        .expect("list_tasks");
    let root = tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .unwrap_or_else(|| panic!("t1 missing: {tasks:?}"));
    assert!(
        state
            .orch
            .store
            .update_task_status(root.id, "queued", "ready", None)
            .await
            .expect("queued→ready"),
        "t1 must be queued after materialize"
    );

    let assignment = state
        .orch
        .store
        .begin_spawn_assignment(
            root.id,
            workspace_id,
            root.run_id,
            &root.worker_profile,
            "remote",
            None,
        )
        .await
        .expect("begin_spawn_assignment")
        .expect("CAS ready→assigned");
    (assignment.session_id, assignment.worker_token)
}

/// Pair + lease a `spawn_session` claim (http_complete ok=false SUT).
struct LeasedSpawnSession {
    machine: String,
    task_id: Uuid,
    session_id: Uuid,
    worker_token: String,
    claim_id: Uuid,
    lease_token: String,
}

async fn plant_leased_spawn_session(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
    host_name: &str,
) -> LeasedSpawnSession {
    let (host_id, machine) = create_pair_and_heartbeat(app, jar, ws, host_name).await;
    let workspace_id = Uuid::parse_str(ws).expect("ws");
    let (session_id, worker_token) = plant_spawning_remote_session(app, state, jar, ws).await;
    let task_id = state
        .orch
        .store
        .get_session(session_id)
        .await
        .expect("get_session")
        .expect("session")
        .board_task_id;
    state
        .orch
        .store
        .record_remote_enqueued(session_id, &format!("host:{host_id}"), None)
        .await
        .expect("enq");
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({
                "session_id": session_id,
                "board_task_id": task_id,
                "host_id": host_id,
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("claim");
    let lease = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert!(
        lease.body["claim"].is_object(),
        "spawn_session claim: {}",
        lease.body
    );
    let lease_token = lease.body["claim"]["lease_token"]
        .as_str()
        .expect("lease_token")
        .to_string();
    LeasedSpawnSession {
        machine,
        task_id,
        session_id,
        worker_token,
        claim_id,
        lease_token,
    }
}

fn session_in(body: &Value, session_id: Uuid) -> &Value {
    let id = session_id.to_string();
    body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["id"].as_str() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("session missing: {body}"))
}

fn session_for_handle<'a>(body: &'a Value, handle: &str) -> &'a Value {
    body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["runtime_handle"].as_str() == Some(handle))
        .unwrap_or_else(|| panic!("session missing for {handle}: {body}"))
}

fn task_in<'a>(body: &'a Value, plan_task_id: &str) -> &'a Value {
    body["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .find(|t| t["plan_task_id"].as_str() == Some(plan_task_id))
        .unwrap_or_else(|| panic!("task {plan_task_id} missing: {body}"))
}

/// Job-token POST /v1/.../heartbeat + tick apply_inbox must promote
/// a spawning remote session to healthy with progress on GET /sessions.
#[tokio::test]
async fn host_mode_job_heartbeat_promotes_session_healthy() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let before = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(before.status, StatusCode::OK, "sessions: {}", before.body);
    let sess = session_in(&before.body, session_id);
    assert_eq!(sess["status"], "spawning", "pre-heartbeat: {}", before.body);
    assert_eq!(
        sess["runtime_backend"], "remote",
        "host-mode session must be remote, not stub: {}",
        before.body
    );
    assert_eq!(sess["progress_pct"], 0, "{}", before.body);
    assert!(
        sess["last_heartbeat_at"].is_null(),
        "last_heartbeat_at: {}",
        before.body
    );

    let hb = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/heartbeat"),
        &job_token,
        Some(json!({ "seq": 1, "progress_pct": 5 })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::ACCEPTED,
        "worker heartbeat: {}",
        hb.body
    );

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    let applied = tick.body["inbox_applied"].as_u64().unwrap_or(0);
    assert!(
        applied >= 1,
        "tick must apply job-token inbox reports: {}",
        tick.body
    );

    let after = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "sessions: {}", after.body);
    let sess = session_in(&after.body, session_id);
    assert_eq!(
        sess["status"], "healthy",
        "apply_heartbeat must promote spawning→healthy: {}",
        after.body
    );
    assert_eq!(sess["progress_pct"], 5, "{}", after.body);
    assert!(
        sess["last_heartbeat_at"].is_string(),
        "last_heartbeat_at: {}",
        after.body
    );
}

/// POST /v1 heartbeat is inbox-only: without /tick the session stays spawning.
#[tokio::test]
async fn job_token_heartbeat_without_tick_leaves_session_spawning() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let hb = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/heartbeat"),
        &job_token,
        Some(json!({ "seq": 1, "progress_pct": 5 })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::ACCEPTED,
        "worker heartbeat: {}",
        hb.body
    );

    let after = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "sessions: {}", after.body);
    let sess = session_in(&after.body, session_id);
    assert_eq!(
        sess["status"], "spawning",
        "ACCEPTED must not apply_heartbeat until tick: {}",
        after.body
    );
    assert!(
        sess["last_heartbeat_at"].is_null(),
        "last_heartbeat_at: {}",
        after.body
    );
}

/// status=running inbox arm calls apply_heartbeat(pct=None); no /heartbeat row.
#[tokio::test]
async fn job_token_status_running_alone_promotes_healthy() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let status = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "running",
            "reason": Value::Null
        })),
    )
    .await;
    assert_eq!(
        status.status,
        StatusCode::ACCEPTED,
        "worker status: {}",
        status.body
    );

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    let applied = tick.body["inbox_applied"].as_u64().unwrap_or(0);
    assert!(
        applied >= 1,
        "tick must apply status=running inbox: {}",
        tick.body
    );

    let after = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "sessions: {}", after.body);
    let sess = session_in(&after.body, session_id);
    assert_eq!(
        sess["status"], "healthy",
        "status=running apply_heartbeat must promote spawning→healthy: {}",
        after.body
    );
    assert_eq!(sess["progress_pct"], 0, "{}", after.body);
}

/// Tick assigns an online advertised_host (remote / host:{id} / spawning)
/// and leaves a spawn_session claim leasable. Remote stays spawning until
/// the host leases + job-heartbeats.
#[tokio::test]
async fn tick_assigns_remote_host_instead_of_local_stub() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "tick-host").await;
    // New workspaces default to container. This host is process-only.
    let pin = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({ "min_isolation": "process" })),
    )
    .await;
    assert_eq!(pin.status, StatusCode::OK, "pin process: {}", pin.body);

    let fleet_proj = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(fleet_proj.status, StatusCode::OK, "{}", fleet_proj.body);

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    assert!(
        tick.body["spawned"].as_u64().unwrap_or(0) >= 1,
        "tick must spawn: {}",
        tick.body
    );

    let handle = format!("host:{host_id}");
    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let sess = session_for_handle(&sessions.body, &handle);
    assert_eq!(
        sess["runtime_backend"], "remote",
        "tick must choose the paired host, not stub: {}",
        sessions.body
    );
    assert_eq!(
        sess["status"], "spawning",
        "remote job stays spawning until the host leases it: {}",
        sessions.body
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
        lease.body["claim"].is_object(),
        "tick must enqueue a claim: {}",
        lease.body
    );
}

/// record_remote_enqueued must not stamp last_heartbeat_at (that would start
/// the stale clock). HTTP tick assign is the path that never read this field.
#[tokio::test]
async fn tick_remote_assign_does_not_stamp_last_heartbeat_at() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "hb-null-host").await;
    let pin = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({ "min_isolation": "process" })),
    )
    .await;
    assert_eq!(pin.status, StatusCode::OK, "pin process: {}", pin.body);

    let fleet_proj = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(fleet_proj.status, StatusCode::OK, "{}", fleet_proj.body);

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let handle = format!("host:{host_id}");
    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let sess = session_for_handle(&sessions.body, &handle);
    assert!(
        sess["last_heartbeat_at"].is_null(),
        "record_remote_enqueued must not stamp last_heartbeat_at: {}",
        sessions.body
    );
}

/// HTTP tick must skip a still-paired paused box. list_assignable and
/// paused-lease already exist; this is assign-skip on POST /fleet/tick.
#[tokio::test]
async fn tick_paused_host_leaves_ready() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "paused-tick").await;

    let paused = call(
        &app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(&ws)),
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

    let fleet_proj = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(fleet_proj.status, StatusCode::OK, "{}", fleet_proj.body);

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);

    let leftover = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let leftover_other = leftover.iter().any(|h| h.id != host_id);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    assert!(
        tick.body["spawned"].as_u64().unwrap_or(0) == 0 || leftover_other,
        "paused host must not take a tick spawn: {}",
        tick.body
    );

    let handle = format!("host:{host_id}");
    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let list = sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        list.iter()
            .all(|s| s["runtime_handle"].as_str() != Some(handle.as_str())),
        "paused host must not get a session: {}",
        sessions.body
    );
}

/// spawn_session complete ok=false: session failed_spawn, first attempt retry_wait.
#[tokio::test]
async fn complete_ok_false_is_spawn_failure_not_success() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let planted = plant_leased_spawn_session(&app, &state, &jar, &ws, "fail-box").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let done = call_bearer(
        &app,
        "POST",
        &format!(
            "/api/worker-hosts/me/lease/{}/complete",
            planted.claim_id
        ),
        &planted.machine,
        Some(json!({
            "lease_token": planted.lease_token,
            "ok": false,
            "error": "exec failed"
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "sessions: {}", sessions.body);
    let sess = session_in(&sessions.body, planted.session_id);
    assert_eq!(
        sess["status"], "failed_spawn",
        "ok=false must mark failed_spawn, not still spawning: {}",
        sessions.body
    );
    assert_eq!(
        sess["exit_reason"], "spawn_failed",
        "complete_spawn_failure exit_reason: {}",
        sessions.body
    );

    let task = state
        .orch
        .store
        .get_task(workspace_id, planted.task_id)
        .await
        .expect("get")
        .expect("task");
    assert_eq!(
        task.status, "retry_wait",
        "attempt 1 must retry_wait, not done_failed/ready: {}",
        task.status
    );
    assert_eq!(task.attempt, 1, "first assign is attempt 1, got {}", task.attempt);
}

/// max_retries=0 (attempt already at cap): ok=false is done_failed / spawn_failed.
#[tokio::test]
async fn complete_ok_false_exhausted_retries_is_done_failed() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let planted = plant_leased_spawn_session(&app, &state, &jar, &ws, "fail-box-cap").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    let capped = sqlx::query("UPDATE board_tasks SET max_retries = 0 WHERE id = $1")
        .bind(planted.task_id)
        .execute(state.orch.store.pool())
        .await
        .expect("max_retries=0");
    assert_eq!(capped.rows_affected(), 1, "must cap this task");

    let done = call_bearer(
        &app,
        "POST",
        &format!(
            "/api/worker-hosts/me/lease/{}/complete",
            planted.claim_id
        ),
        &planted.machine,
        Some(json!({
            "lease_token": planted.lease_token,
            "ok": false,
            "error": "exec failed"
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);

    let task = state
        .orch
        .store
        .get_task(workspace_id, planted.task_id)
        .await
        .expect("get")
        .expect("task");
    assert_eq!(
        task.status, "done_failed",
        "exhausted spawn fail must be done_failed, not retry_wait: {}",
        task.status
    );
    assert_eq!(
        task.failure_reason.as_deref(),
        Some("spawn_failed"),
        "failure_reason: {:?}",
        task.failure_reason
    );
}

/// After ok=false, the spawn job token must not ACCEPTED /v1 heartbeat.
#[tokio::test]
async fn complete_ok_false_rejects_later_job_token_heartbeat() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let planted = plant_leased_spawn_session(&app, &state, &jar, &ws, "fail-box-hb").await;

    let done = call_bearer(
        &app,
        "POST",
        &format!(
            "/api/worker-hosts/me/lease/{}/complete",
            planted.claim_id
        ),
        &planted.machine,
        Some(json!({
            "lease_token": planted.lease_token,
            "ok": false,
            "error": "exec failed"
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);

    let hb = call_bearer(
        &app,
        "POST",
        &format!(
            "/v1/projects/{ws}/workers/{}/heartbeat",
            planted.session_id
        ),
        &planted.worker_token,
        Some(json!({ "seq": 1, "progress_pct": 5 })),
    )
    .await;
    assert_ne!(
        hb.status,
        StatusCode::ACCEPTED,
        "failed_spawn must not accept job-token heartbeat: {}",
        hb.body
    );
    assert_eq!(
        hb.status,
        StatusCode::UNAUTHORIZED,
        "authenticate_worker_session rejects failed_spawn: {}",
        hb.body
    );
}

#[tokio::test]
async fn signal_session_complete_does_not_spawn_fail() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "sig-box").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    let plan_id = Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan");
    state
        .orch
        .store
        .materialize_plan(workspace_id, plan_id)
        .await
        .expect("materialize");
    let tasks = state
        .orch
        .store
        .list_tasks_slim(workspace_id, None)
        .await
        .expect("tasks");
    let root = tasks.iter().find(|t| t.plan_task_id == "t1").expect("t1");
    state
        .orch
        .store
        .update_task_status(root.id, "queued", "ready", None)
        .await
        .expect("ready");
    let assignment = state
        .orch
        .store
        .begin_spawn_assignment(
            root.id,
            workspace_id,
            root.run_id,
            &root.worker_profile,
            "remote",
            None,
        )
        .await
        .expect("assign")
        .expect("cas");
    let session_id = assignment.session_id;
    let before = state
        .orch
        .store
        .get_task(workspace_id, root.id)
        .await
        .expect("get")
        .expect("task");
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "signal_session".into(),
            payload: json!({
                "kind": "signal_session",
                "session_id": session_id,
                "board_task_id": root.id,
                "host_id": host_id,
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("claim");
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "{}", lease.body);
    assert_eq!(
        lease.body["claim"]["payload"]["kind"].as_str(),
        Some("signal_session")
    );
    let token = lease.body["claim"]["lease_token"].as_str().expect("token");
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({ "lease_token": token, "ok": false, "error": "no live child" })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let after = state
        .orch
        .store
        .get_task(workspace_id, root.id)
        .await
        .expect("get")
        .expect("task");
    assert_eq!(
        after.status, before.status,
        "signal complete must not apply spawn failure"
    );
}

#[tokio::test]
async fn cluster_host_is_not_assignable_in_other_workspace() {
    let (app, state) = app_with_state().await;
    let (_a, jar_a, ws_a) = signup_and_login(&app).await;
    let (host_id, _machine) = create_pair_and_heartbeat(&app, &jar_a, &ws_a, "cluster").await;
    fleet_db::repos::worker_hosts::patch_cp_settings(
        &state.db,
        host_id,
        &fleet_db::models::HostCpSettings {
            name: None,
            enabled: None,
            paused: None,
            drain: None,
            isolation: None,
            allow_host: None,
            allow_container: None,
            max_concurrent: None,
            max_job_timeout_s: None,
            labels: Some(vec!["cluster".into()]),
            auto_update: None,
            request_update: None,
            install_model: None,
            runtime_settings: None,
        },
    )
    .await
    .expect("label cluster");
    let labeled = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("reload host")
        .expect("host");
    assert!(
        labeled.is_cluster_host(),
        "test host must carry the cluster label: {:?}",
        labeled.labels
    );
    let (_b, _jar_b, ws_b) = signup_and_login(&app).await;
    let ws_b_id = Uuid::parse_str(&ws_b).expect("ws b");
    let from_b = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        ws_b_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list B");
    assert!(
        from_b.iter().all(|h| h.id != host_id),
        "cluster host must not be assignable in a foreign workspace"
    );
    let picked = fleet_orchestrator::store::select_assignable_runner_preferring(
        &state.orch.store,
        ws_b_id,
        None,
        "http://127.0.0.1:8080",
    )
    .await;
    assert!(
        picked.as_ref().map(|c| c.id() != host_id).unwrap_or(true),
        "picker must not send foreign workspace work to the cluster host"
    );
}

/// Job-token POST /v1 status=success + result_md then cookie /tick must
/// apply_inbox → validate_done → GET /fleet/tasks t1=done_success.
/// Chain t1 acceptance is free-form "artifact exists", not exists:work.
#[tokio::test]
async fn remote_success_with_result_md_is_done() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let status = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "result_md": "# Result\n\nACP end_turn. hello from mock agent.\n",
        })),
    )
    .await;
    assert_eq!(status.status, StatusCode::ACCEPTED, "{}", status.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "tasks: {}", tasks.body);
    assert_eq!(
        task_in(&tasks.body, "t1")["status"],
        "done_success",
        "claiming remote + result_md ≥20 must pass validate_done: {}",
        tasks.body
    );
}

/// POST /v1 status=success+result_md is inbox-only: without /tick
/// GET /fleet/tasks stays not done_success.
#[tokio::test]
async fn job_token_status_success_without_tick_leaves_not_done() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let status = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "result_md": "# Result\n\nACP end_turn. hello from mock agent.\n",
        })),
    )
    .await;
    assert_eq!(status.status, StatusCode::ACCEPTED, "{}", status.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "tasks: {}", tasks.body);
    assert_ne!(
        task_in(&tasks.body, "t1")["status"],
        "done_success",
        "ACCEPTED must not apply_task_terminal until tick: {}",
        tasks.body
    );
}

/// Remote success with result_md under validate_done's 20-char floor
/// must not become done_success after /tick.
#[tokio::test]
async fn remote_success_result_md_under_20_chars_is_not_done() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (session_id, job_token) = plant_spawning_remote_session(&app, &state, &jar, &ws).await;

    let status = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "result_md": "short result",
        })),
    )
    .await;
    assert_eq!(status.status, StatusCode::ACCEPTED, "{}", status.body);

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "tasks: {}", tasks.body);
    assert_ne!(
        task_in(&tasks.body, "t1")["status"],
        "done_success",
        "result_md under 20 chars must fail validate_done: {}",
        tasks.body
    );
}

#[tokio::test]
async fn complete_rejects_bad_machine_token() {
    let (app, _) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_host_id, _machine) = create_pair_and_heartbeat(&app, &jar, &ws, "tok-box").await;
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{}/complete", Uuid::new_v4()),
        "flm_not-a-real-token",
        Some(json!({
            "lease_token": Uuid::new_v4().to_string(),
            "ok": true
        })),
    )
    .await;
    assert_eq!(done.status, StatusCode::UNAUTHORIZED, "{}", done.body);
}

#[tokio::test]
async fn complete_rejects_wrong_lease_token() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "lease-box").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    let plan_id = Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan");
    state
        .orch
        .store
        .materialize_plan(workspace_id, plan_id)
        .await
        .expect("materialize");
    let tasks = state
        .orch
        .store
        .list_tasks_slim(workspace_id, None)
        .await
        .expect("tasks");
    let root = tasks.iter().find(|t| t.plan_task_id == "t1").expect("t1");
    state
        .orch
        .store
        .update_task_status(root.id, "queued", "ready", None)
        .await
        .expect("ready");
    state
        .orch
        .store
        .begin_spawn_assignment(
            root.id,
            workspace_id,
            root.run_id,
            &root.worker_profile,
            "remote",
            None,
        )
        .await
        .expect("assign")
        .expect("cas");
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({ "session_id": Uuid::new_v4(), "host_id": host_id }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("claim");
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "{}", lease.body);
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({
            "lease_token": Uuid::new_v4().to_string(),
            "ok": true
        })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::CONFLICT,
        "wrong lease token must not complete: {}",
        done.body
    );
}

#[tokio::test]
async fn complete_rejects_another_hosts_claim() {
    let (app, state) = app_with_state().await;
    let (_a, jar, ws) = signup_and_login(&app).await;
    let (host_a, machine_a) = create_pair_and_heartbeat(&app, &jar, &ws, "box-a").await;
    let (_host_b, machine_b) = create_pair_and_heartbeat(&app, &jar, &ws, "box-b").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({ "session_id": Uuid::new_v4(), "host_id": host_a }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("claim");
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine_a,
        Some(json!({})),
    )
    .await;
    assert_eq!(lease.status, StatusCode::OK, "{}", lease.body);
    let token = lease.body["claim"]["lease_token"].as_str().expect("token");
    let stolen = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine_b,
        Some(json!({ "lease_token": token, "ok": true })),
    )
    .await;
    assert!(
        stolen.status == StatusCode::CONFLICT
            || stolen.status == StatusCode::FORBIDDEN
            || stolen.status == StatusCode::UNAUTHORIZED
            || stolen.status == StatusCode::NOT_FOUND,
        "host B must not complete host A's lease: {} {}",
        stolen.status,
        stolen.body
    );
}

#[tokio::test]
async fn signal_after_done_success_does_not_revert() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "sig-done").await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    let plan_id = Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan");
    state
        .orch
        .store
        .materialize_plan(workspace_id, plan_id)
        .await
        .expect("materialize");
    let tasks = state
        .orch
        .store
        .list_tasks_slim(workspace_id, None)
        .await
        .expect("tasks");
    let root = tasks.iter().find(|t| t.plan_task_id == "t1").expect("t1");
    state
        .orch
        .store
        .update_task_status(root.id, "queued", "ready", None)
        .await
        .expect("ready");
    let assignment = state
        .orch
        .store
        .begin_spawn_assignment(
            root.id,
            workspace_id,
            root.run_id,
            &root.worker_profile,
            "remote",
            None,
        )
        .await
        .expect("assign")
        .expect("cas");
    state
        .orch
        .store
        .apply_task_terminal(root.id, assignment.session_id, "done_success", None, 1)
        .await
        .expect("done");
    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "signal_session".into(),
            payload: json!({
                "kind": "signal_session",
                "session_id": assignment.session_id,
                "board_task_id": root.id,
                "host_id": host_id,
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("claim");
    let lease = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        &machine,
        Some(json!({})),
    )
    .await;
    let token = lease.body["claim"]["lease_token"].as_str().expect("token");
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({ "lease_token": token, "ok": false, "error": "already gone" })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let after = state
        .orch
        .store
        .get_task(workspace_id, root.id)
        .await
        .expect("get")
        .expect("task");
    assert_eq!(
        after.status, "done_success",
        "signal after done_success must not reopen or fail the task"
    );
}

#[tokio::test]
async fn min_isolation_container_does_not_assign_process_only_host() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, _) = create_pair_and_heartbeat(&app, &jar, &ws, "proc-only").await;
    call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    let patched = call(
        &app,
        "PATCH",
        &fleet(&ws, "/settings"),
        Some(&jar),
        Some(json!({ "min_isolation": "container" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["min_isolation"], "container");
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
    assert_eq!(tick.status, StatusCode::OK, "{}", tick.body);
    // The API does not local-spawn. A process-only paired host must not
    // take a container-isolation job.
    let workspace_id = Uuid::parse_str(&ws).expect("ws");
    let sessions = state
        .orch
        .store
        .load_board_snapshot(workspace_id)
        .await
        .expect("snap");
    let handle = format!("host:{host_id}");
    assert!(
        sessions
            .sessions
            .iter()
            .all(|s| s.runtime_handle.as_deref() != Some(handle.as_str())),
        "process-only host must not be chosen for container need: {:?}",
        sessions.sessions
    );
}
