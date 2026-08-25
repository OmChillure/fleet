//! Host-mode e2e: pair → lease → job-token /v1 heartbeat → session healthy.
//!
//! Oneshot only (no live bind, no `fleet-host` child). Requires DATABASE_URL
//! — never skips.

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
        process_worker_argv: vec!["fleet-driver".into()],
        k8s: fleet_orchestrator::sandbox::K8sOpts::default(),
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
    #[allow(dead_code)]
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

fn fleet(ws: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("/api/workspaces/{ws}/fleet")
    } else {
        format!("/api/workspaces/{ws}/fleet{suffix}")
    }
}

/// Pair + machine heartbeat so the host is online with `advertised_host=true`.
async fn create_pair_and_heartbeat(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    name: &str,
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

/// Host-mode proof: leased spawn_session + fleet-worker protocol → healthy.
///
/// Create/pair/lease already exists in `worker_hosts_http.rs`. The gap this
/// covers is job-token `POST /v1/.../heartbeat` + tick inbox so
/// `apply_heartbeat` promotes `spawning` → `healthy`.
#[tokio::test]
async fn host_mode_job_heartbeat_promotes_session_healthy() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "studio").await;

    let fleet_proj = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        fleet_proj.status,
        StatusCode::OK,
        "ensure fleet project: {}",
        fleet_proj.body
    );
    assert_eq!(
        fleet_proj.body["project_id"].as_str(),
        Some(ws.as_str()),
        "workspace_id == project_id: {}",
        fleet_proj.body
    );

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "fixture: {}", fx.body);
    let plan_id =
        Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

    // Materialize + queued→ready without ticking (a tick would also spawn).
    // Then insert a spawning remote session with a known job token and enqueue
    // the spawn_session claim the host will lease — same shape as tick spawn.
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
    let session_id = assignment.session_id;
    let job_token = assignment.worker_token.clone();
    state
        .orch
        .store
        .record_remote_enqueued(session_id, &format!("host:{host_id}"), None)
        .await
        .expect("record_remote_enqueued");

    let claim_id = Uuid::new_v4();
    fleet_db::repos::work_claims::enqueue(
        &state.db,
        &fleet_db::NewWorkClaim {
            id: claim_id,
            workspace_id,
            kind: "spawn_session".into(),
            payload: json!({
                "kind": "spawn_session",
                "session_id": session_id,
                "project_id": workspace_id,
                "run_id": root.run_id,
                "board_task_id": root.id,
                "host_id": host_id,
                "worker_token": job_token,
                "runtime": "host",
                "orchestrator_url": "http://127.0.0.1:0",
                "env": { "FLEET_WORKER_TOKEN": job_token }
            }),
            max_attempts: 3,
            available_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("enqueue");

    let before = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(before.status, StatusCode::OK, "sessions: {}", before.body);
    let sess = before.body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["id"].as_str() == Some(session_id.to_string().as_str()))
        .unwrap_or_else(|| panic!("session missing: {}", before.body));
    assert_eq!(sess["status"], "spawning", "pre-heartbeat: {}", before.body);
    assert_eq!(
        sess["runtime_backend"], "remote",
        "host-mode session must be remote, not stub: {}",
        before.body
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
    let claim = lease.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("expected claim: {}", lease.body));
    assert_eq!(claim["id"].as_str(), Some(claim_id.to_string().as_str()));
    assert_eq!(
        claim["payload"]["session_id"].as_str(),
        Some(session_id.to_string().as_str()),
        "lease payload.session_id: {}",
        lease.body
    );
    assert_eq!(
        claim["payload"]["worker_token"].as_str(),
        Some(job_token.as_str()),
        "lease payload.worker_token: {}",
        lease.body
    );
    assert_eq!(claim["payload"]["runtime"], "host");
    assert_eq!(
        claim["payload"]["host_id"].as_str(),
        Some(host_id.to_string().as_str())
    );
    let lease_token = claim["lease_token"].as_str().expect("lease_token");

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

    // Simulate the child worker (fleet-worker protocol) against orch fallback.
    let worker_base = format!("/v1/projects/{ws}/workers/{session_id}");
    let status = call_bearer(
        &app,
        "POST",
        &format!("{worker_base}/status"),
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

    let hb = call_bearer(
        &app,
        "POST",
        &format!("{worker_base}/heartbeat"),
        &job_token,
        Some(json!({ "seq": 2, "progress_pct": 5 })),
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
    let sess = after.body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["id"].as_str() == Some(session_id.to_string().as_str()))
        .unwrap_or_else(|| panic!("session missing after tick: {}", after.body));
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

/// Tick must enqueue on a live host instead of spawning the local stub.
/// stub_duration is 0 in this fixture: a local spawn would complete in the
/// same tick. Remote stays `spawning` until the host leases + job-heartbeats.
#[tokio::test]
async fn tick_assigns_remote_host_instead_of_local_stub() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "tick-host").await;

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
    assert_eq!(
        tick.body["completed_via_runtime"].as_u64().unwrap_or(0),
        0,
        "remote session must not be completed by the local stub: {}",
        tick.body
    );

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let sess = sessions.body["sessions"]
        .as_array()
        .expect("sessions")
        .first()
        .unwrap_or_else(|| panic!("expected a session: {}", sessions.body));
    assert_eq!(
        sess["runtime_backend"], "remote",
        "tick must choose the paired host, not stub: {}",
        sessions.body
    );
    assert_eq!(
        sess["runtime_handle"].as_str(),
        Some(format!("host:{host_id}").as_str()),
        "{}",
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
    let claim = lease.body["claim"]
        .as_object()
        .unwrap_or_else(|| panic!("tick must enqueue a claim: {}", lease.body));
    assert_eq!(claim["payload"]["runtime"], "host");
    assert_eq!(
        claim["payload"]["host_id"].as_str(),
        Some(host_id.to_string().as_str())
    );
    assert_eq!(claim["payload"]["session_id"].as_str(), sess["id"].as_str());
    let token = claim["payload"]["worker_token"].as_str().unwrap_or("");
    assert!(!token.is_empty(), "job token must be on the payload");
    assert!(
        claim["payload"]["env"]
            .as_object()
            .map(|e| e.get("FLEET_WORK_DIR").is_none())
            .unwrap_or(true),
        "CP work_dir must not leave the plane: {}",
        lease.body
    );
}

#[tokio::test]
async fn tick_without_host_leaves_ready() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("ws");

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

    let assignable = fleet_db::repos::worker_hosts::list_assignable(
        &state.db,
        workspace_id,
        chrono::Duration::seconds(90),
    )
    .await
    .expect("list_assignable");

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let list = sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    if assignable.is_empty() {
        assert_eq!(
            tick.body["spawned"].as_u64().unwrap_or(0),
            0,
            "no assignable host → production must not local-spawn: {}",
            tick.body
        );
        assert!(
            list.is_empty(),
            "no assignable host → no session: {}",
            sessions.body
        );
    } else {
        // A cluster host from another workspace is assignable by product.
        // That is not a license to local-spawn stub.
        for s in &list {
            assert_eq!(
                s["runtime_backend"], "remote",
                "hostless workspace must not fall back to stub/process: {}",
                sessions.body
            );
        }
    }
}

#[tokio::test]
async fn complete_ok_false_is_spawn_failure_not_success() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "fail-box").await;
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
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
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
        Some(json!({ "lease_token": token, "ok": false, "error": "exec failed" })),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let task = state
        .orch
        .store
        .get_task(workspace_id, root.id)
        .await
        .expect("get")
        .expect("task");
    assert_ne!(
        task.status, "done_success",
        "ok=false must not succeed: {}",
        task.status
    );
    assert_ne!(
        task.status, "in_progress",
        "ok=false must not look like a live job: {}",
        task.status
    );
    assert!(
        matches!(
            task.status.as_str(),
            "retry_wait" | "done_failed" | "failed" | "queued" | "ready"
        ),
        "ok=false must apply spawn-failure policy, got {}",
        task.status
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
async fn cluster_host_completes_claim_in_other_workspace() {
    let (app, state) = app_with_state().await;
    let (_a, jar_a, ws_a) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar_a, &ws_a, "cluster").await;
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
    let (_b, jar_b, ws_b) = signup_and_login(&app).await;
    let ws_b_id = Uuid::parse_str(&ws_b).expect("ws b");
    call(&app, "GET", &fleet(&ws_b, ""), Some(&jar_b), None).await;
    let fx = call(
        &app,
        "POST",
        &fleet(&ws_b, "/runs/fixture"),
        Some(&jar_b),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    let plan_id = Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan");
    state
        .orch
        .store
        .materialize_plan(ws_b_id, plan_id)
        .await
        .expect("materialize");
    let tasks = state
        .orch
        .store
        .list_tasks_slim(ws_b_id, None)
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
            ws_b_id,
            root.run_id,
            &root.worker_profile,
            "remote",
            None,
        )
        .await
        .expect("assign")
        .expect("cas");
    let session_id = assignment.session_id;
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
            workspace_id: ws_b_id,
            kind: "spawn_session".into(),
            payload: json!({
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
    assert_eq!(lease.status, StatusCode::OK, "lease: {}", lease.body);
    assert_eq!(
        lease.body["claim"]["id"].as_str(),
        Some(claim_id.to_string().as_str()),
        "{}",
        lease.body
    );
    let token = lease.body["claim"]["lease_token"].as_str().expect("token");
    let done = call_bearer(
        &app,
        "POST",
        &format!("/api/worker-hosts/me/lease/{claim_id}/complete"),
        &machine,
        Some(json!({ "lease_token": token, "ok": true })),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "complete cross-ws: {}",
        done.body
    );
    let home_task = state
        .orch
        .store
        .get_task(Uuid::parse_str(&ws_a).expect("ws a"), root.id)
        .await
        .expect("get home");
    assert!(
        home_task.is_none(),
        "cluster complete must not write the host's home workspace: {home_task:?}"
    );
    let foreign = state
        .orch
        .store
        .get_task(ws_b_id, root.id)
        .await
        .expect("get foreign")
        .expect("task lives in the claim workspace");
    assert_ne!(
        foreign.status, "done_success",
        "lease complete is not proof of work; inbox/validate_done is: {}",
        foreign.status
    );

    let job_token = assignment.worker_token.clone();
    let worker_base = format!("/v1/projects/{ws_b}/workers/{session_id}");
    let status = call_bearer(
        &app,
        "POST",
        &format!("{worker_base}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "reason": Value::Null
        })),
    )
    .await;
    assert_eq!(status.status, StatusCode::ACCEPTED, "{}", status.body);
    let tick = call(&app, "POST", &fleet(&ws_b, "/tick"), Some(&jar_b), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    let after = state
        .orch
        .store
        .get_task(ws_b_id, root.id)
        .await
        .expect("get after")
        .expect("task");
    assert_ne!(
        after.status, "done_success",
        "remote guest success without result.md / artifacts is not done: {}",
        after.status
    );

    let stale = call_bearer(
        &app,
        "POST",
        &format!("{worker_base}/status"),
        &job_token,
        Some(json!({
            "seq": 2,
            "status": "success",
            "result_md": "# Result\n\nACP end_turn. hello from mock agent.\nWork is in work/.\n",
            "files": { "work/.keep": "ok" }
        })),
    )
    .await;
    assert_eq!(stale.status, StatusCode::ACCEPTED, "{}", stale.body);
    let tick2 = call(&app, "POST", &fleet(&ws_b, "/tick"), Some(&jar_b), None).await;
    assert_eq!(tick2.status, StatusCode::OK, "tick2: {}", tick2.body);
    let after_stale = state
        .orch
        .store
        .get_task(ws_b_id, root.id)
        .await
        .expect("get after stale")
        .expect("task");
    assert_ne!(
        after_stale.status, "done_success",
        "success from a session that no longer holds the claim must not complete: {}",
        after_stale.status
    );
}

#[tokio::test]
async fn remote_success_with_result_md_is_done() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (host_id, machine) = create_pair_and_heartbeat(&app, &jar, &ws, "proof-box").await;
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
    let job_token = assignment.worker_token.clone();
    let status = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{session_id}/status"),
        &job_token,
        Some(json!({
            "seq": 1,
            "status": "success",
            "result_md": "# Result\n\nACP end_turn. hello from mock agent.\n",
            "files": { "work/.keep": "ok" }
        })),
    )
    .await;
    assert_eq!(status.status, StatusCode::ACCEPTED, "{}", status.body);
    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);
    let proven = state
        .orch
        .store
        .get_task(workspace_id, root.id)
        .await
        .expect("get")
        .expect("task");
    assert_eq!(
        proven.status, "done_success",
        "guest result.md + work/ on the claiming session must pass validate_done: {}",
        proven.status
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
    assert_eq!(
        tick.body["spawned"].as_u64().unwrap_or(0),
        0,
        "process-only host must not take a container workspace: {}",
        tick.body
    );
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
