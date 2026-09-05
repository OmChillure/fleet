//! GET /inbox?state= filters by ticket state; unknown state is not 500.
//! Requires DATABASE_URL — never skips.
//!
//! Helpers are copied from `http_api.rs` (`http_api.rs` is exclusively
//! locked by another writer; do not import private items).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use fleet_auth::{ACCESS_COOKIE, AuthConfig, REFRESH_COOKIE};
use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox};
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

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
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

fn inbox_path(ws: &str, query: &str) -> String {
    if query.is_empty() {
        format!("/api/workspaces/{ws}/inbox")
    } else {
        format!("/api/workspaces/{ws}/inbox?{query}")
    }
}

fn inbox_keys(body: &Value) -> Vec<String> {
    body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {body}"))
        .iter()
        .filter_map(|t| t["key"].as_str().map(str::to_string))
        .collect()
}

fn inbox_ticket<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {body}"))
        .iter()
        .find(|t| t["key"].as_str() == Some(key))
        .unwrap_or_else(|| panic!("missing key {key}: {body}"))
}

fn assert_inbox_not_500(out: &HttpOut, label: &str) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{label} must not 500: {} {}",
        out.status,
        out.body
    );
}

fn assert_no_ticket_leak(out: &HttpOut, keys: &[&str], label: &str) {
    let blob = format!("{} {}", out.body, out.raw);
    for key in keys {
        assert!(
            !blob.contains(key),
            "{label} must not leak ticket {key}: {} {}",
            out.status,
            out.body
        );
    }
    if let Some(tickets) = out.body["tickets"].as_array() {
        assert!(
            tickets.is_empty(),
            "{label} must not return inbox tickets: {}",
            out.body
        );
    }
}

fn assert_inbox_exact_keys(out: &HttpOut, expected: &[&str], label: &str) {
    assert_inbox_not_500(out, label);
    assert_eq!(out.status, StatusCode::OK, "{label}: {}", out.body);
    let mut keys = inbox_keys(&out.body);
    keys.sort();
    let mut want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    want.sort();
    assert_eq!(keys, want, "{label} keys: {}", out.body);
    let n = out.body["tickets"].as_array().map(Vec::len).unwrap_or(0);
    assert_eq!(
        out.body["count"].as_u64(),
        Some(n as u64),
        "{label} count must match tickets.len(): {}",
        out.body
    );
}

async fn seed_inbox(state: &AppState, workspace_id: Uuid, key: &str, title: &str, closed: bool) {
    ticket_repo::upsert_inbox(
        &state.db,
        &UpsertInbox {
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
    .unwrap_or_else(|e| panic!("seed {key}: {e}"));
}

/// GET /inbox?state= keeps only matching tickets. Unknown / junk state is
/// 200 with an empty list — not 500. Empty/whitespace `state` is unfiltered.
#[tokio::test]
async fn inbox_get_state_query_filters_and_unknown_does_not_500() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let b_ws_id = Uuid::parse_str(&b_ws).expect("B workspace id");
    let tag = Uuid::new_v4();
    let open_key = format!("state-q-{tag}/app#open");
    let launched_key = format!("state-q-{tag}/app#launched");
    let done_key = format!("state-q-{tag}/app#done");
    let other_key = format!("state-q-{tag}/other#open");

    seed_inbox(&state, a_ws_id, &open_key, "open ticket", false).await;
    seed_inbox(&state, a_ws_id, &launched_key, "launched ticket", false).await;
    let run_id = Uuid::now_v7();
    ticket_repo::mark_launched(&state.db, a_ws_id, "github", &launched_key, run_id)
        .await
        .expect("mark launched")
        .expect("launched row");
    seed_inbox(&state, a_ws_id, &done_key, "done ticket", true).await;
    seed_inbox(&state, b_ws_id, &other_key, "B open ticket", false).await;

    let all = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_inbox_exact_keys(
        &all,
        &[&open_key, &launched_key, &done_key],
        "unfiltered GET",
    );
    assert_eq!(
        inbox_ticket(&all.body, &open_key)["state"].as_str(),
        Some("open"),
        "open ticket: {}",
        all.body
    );
    assert_eq!(
        inbox_ticket(&all.body, &launched_key)["state"].as_str(),
        Some("launched"),
        "launched ticket: {}",
        all.body
    );
    assert_eq!(
        inbox_ticket(&all.body, &launched_key)["run_id"].as_str(),
        Some(run_id.to_string().as_str()),
        "launched run_id: {}",
        all.body
    );
    assert_eq!(
        inbox_ticket(&all.body, &done_key)["state"].as_str(),
        Some("done"),
        "done ticket: {}",
        all.body
    );
    assert!(
        !inbox_keys(&all.body).iter().any(|k| k == &other_key),
        "A unfiltered must not list B's ticket: {}",
        all.body
    );

    let open = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state=open"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(&open, &[&open_key], "GET ?state=open");
    assert_eq!(
        inbox_ticket(&open.body, &open_key)["state"].as_str(),
        Some("open"),
        "{}",
        open.body
    );

    let launched = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state=launched"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(&launched, &[&launched_key], "GET ?state=launched");
    assert_eq!(
        inbox_ticket(&launched.body, &launched_key)["state"].as_str(),
        Some("launched"),
        "{}",
        launched.body
    );

    let done = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state=done"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(&done, &[&done_key], "GET ?state=done");
    assert_eq!(
        inbox_ticket(&done.body, &done_key)["state"].as_str(),
        Some("done"),
        "{}",
        done.body
    );

    let empty_param = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state="),
        Some(&a_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(
        &empty_param,
        &[&open_key, &launched_key, &done_key],
        "GET ?state= (empty) is unfiltered",
    );

    let ws_param = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state=%20%20"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(
        &ws_param,
        &[&open_key, &launched_key, &done_key],
        "GET ?state=whitespace is unfiltered",
    );

    for (query, label) in [
        ("state=not_a_real_state", "unknown name"),
        ("state=OPEN", "wrong case"),
        ("state=ignored", "valid CHECK state with no rows"),
        ("state=open%27%20OR%201%3D1", "sql-injection-shaped"),
        ("state=../../../etc/passwd", "path-shaped"),
    ] {
        let out = call(&app, "GET", &inbox_path(&a_ws, query), Some(&a_jar), None).await;
        assert_inbox_exact_keys(&out, &[], &format!("GET ?{query} ({label})"));
    }

    let after_unknown = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_inbox_exact_keys(
        &after_unknown,
        &[&open_key, &launched_key, &done_key],
        "unknown filter must not drop rows",
    );

    let anon = call(&app, "GET", &inbox_path(&a_ws, "state=open"), None, None).await;
    assert_inbox_not_500(&anon, "anon GET ?state=open");
    assert_eq!(
        anon.status,
        StatusCode::UNAUTHORIZED,
        "anon GET ?state=open must 401: {}",
        anon.body
    );
    assert_no_ticket_leak(&anon, &[&open_key, &launched_key, &done_key], "anon");

    let as_b = call(
        &app,
        "GET",
        &inbox_path(&a_ws, "state=open"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_inbox_not_500(&as_b, "B GET A's ?state=open");
    assert!(
        as_b.status == StatusCode::FORBIDDEN || as_b.status == StatusCode::UNAUTHORIZED,
        "B GET A's ?state=open must be 403/401: {} {}",
        as_b.status,
        as_b.body
    );
    assert_no_ticket_leak(&as_b, &[&open_key, &launched_key, &done_key], "B on A");

    let b_open = call(
        &app,
        "GET",
        &inbox_path(&b_ws, "state=open"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_inbox_exact_keys(&b_open, &[&other_key], "B GET own ?state=open");
    assert!(
        !inbox_keys(&b_open.body)
            .iter()
            .any(|k| k == &open_key || k == &launched_key || k == &done_key),
        "B's filter must not list A's tickets: {}",
        b_open.body
    );
}
