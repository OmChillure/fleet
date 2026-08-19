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
use fleet_server::{AppState, GrokConfig, OllamaConfig, ServerConfig, build_http_app};
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

/// In-process app + state (copied from http_api.rs; do not import private items).
async fn app_with_state() -> (axum::Router, AppState) {
    app_with_state_dist(None).await
}

async fn app_with_state_dist(worker_dist_dir: Option<PathBuf>) -> (axum::Router, AppState) {
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
    assert!(got.body.get("machine_token_hash").is_none());
    assert!(got.body.get("pair_token").is_none());
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
async fn install_worker_bin_dotdot_rejected() {
    let app = app().await;
    let out = call(&app, "GET", "/install/worker/bin/../fleet-host", None, None).await;
    assert!(
        out.status == StatusCode::BAD_REQUEST || out.status == StatusCode::NOT_FOUND,
        "{}",
        out.raw
    );
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
