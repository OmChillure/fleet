//! Fail-closed HTTP tests for `POST /api/workspaces/{id}/chat/stream`.
//! Requires DATABASE_URL — never skips.
//!
//! Helpers are copied from `http_api.rs` (that file is owned by a sibling).

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

/// Stream must reject or emit an error well before any LLM connect timeout.
const FAIL_CLOSED: Duration = Duration::from_secs(5);

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
    format!("chatstream.{}@example.test", Uuid::new_v4())
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

/// In-process app + state (copied from `http_api.rs`; do not import private items).
/// All LLM providers are off / unconfigured so chat cannot hang on a network call.
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
            auth_path: PathBuf::from("/tmp/fleet-server-chat-stream-http-no-grok-auth.json"),
            api_key: None,
            api_base: "https://api.x.ai/v1".into(),
        },
        openai: OpenaiConfig {
            enabled: false,
            api_key: None,
            api_base: "https://api.openai.com/v1".into(),
        },
        claude: ClaudeConfig {
            enabled: false,
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

fn stream_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/stream")
}

fn ndjson_events(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn post_stream_timed(app: &axum::Router, jar: &CookieJar, ws: &str, body: Value) -> HttpOut {
    tokio::time::timeout(
        FAIL_CLOSED,
        call(app, "POST", &stream_path(ws), Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "chat/stream hung for more than {FAIL_CLOSED:?} — disabled providers must fail closed"
        )
    })
}

fn assert_provider_fail_closed(out: &HttpOut, provider: &str) {
    assert!(
        !out.status.is_server_error(),
        "disabled `{provider}` must not 5xx: {} {}",
        out.status,
        out.body
    );
    if out.status == StatusCode::OK {
        let events = ndjson_events(&out.raw);
        assert!(
            events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("error")),
            "disabled `{provider}` stream must emit type=error (no hang / no fake turn): {}",
            out.raw
        );
        assert!(
            !events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("done")),
            "disabled `{provider}` must not complete a chat turn: {}",
            out.raw
        );
    } else {
        assert!(
            out.status.is_client_error(),
            "disabled `{provider}` must be 4xx or NDJSON error, not {} {}",
            out.status,
            out.body
        );
    }
}

fn user_hi() -> Value {
    json!([{ "role": "user", "content": "hi" }])
}

#[tokio::test]
async fn chat_stream_other_workspace_forbidden() {
    let app = app().await;
    let (_a_email, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");

    let out = post_stream_timed(
        &app,
        &b_jar,
        &a_ws,
        json!({
            "messages": user_hi(),
            "provider": "openai"
        }),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "other-workspace chat/stream must be 403, not a stream: {} {}",
        out.status,
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("member") || err.contains("workspace") || err.contains("forbidden"),
        "403 body should name membership: {}",
        out.body
    );
    assert!(
        !out.raw.contains("\"type\":\"done\""),
        "must not stream a turn for another workspace: {}",
        out.raw
    );

    let missing = post_stream_timed(
        &app,
        &b_jar,
        &Uuid::new_v4().to_string(),
        json!({
            "messages": user_hi(),
            "provider": "ollama"
        }),
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::FORBIDDEN,
        "unknown workspace must be 403, not a hang or 200: {} {}",
        missing.status,
        missing.body
    );
}

#[tokio::test]
async fn chat_stream_empty_messages_bad_request() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let empty = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({ "messages": [], "provider": "ollama" }),
    )
    .await;
    assert_eq!(
        empty.status,
        StatusCode::BAD_REQUEST,
        "empty messages must be 400: {}",
        empty.body
    );
    let err = empty.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("message"),
        "400 should mention messages: {}",
        empty.body
    );

    let blank = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": "   " }],
            "provider": "ollama"
        }),
    )
    .await;
    assert_eq!(
        blank.status,
        StatusCode::BAD_REQUEST,
        "whitespace-only messages must be 400 (no stream / no hang): {}",
        blank.body
    );
}

#[tokio::test]
async fn chat_stream_disabled_providers_fail_closed_quickly() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for provider in ["ollama", "grok", "openai", "claude"] {
        let out = post_stream_timed(
            &app,
            &jar,
            &ws,
            json!({
                "messages": user_hi(),
                "provider": provider
            }),
        )
        .await;
        assert_provider_fail_closed(&out, provider);
    }

    let defaulted = post_stream_timed(&app, &jar, &ws, json!({ "messages": user_hi() })).await;
    assert_provider_fail_closed(&defaulted, "default-ollama");

    let unknown = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": user_hi(),
            "provider": "not-a-real-provider"
        }),
    )
    .await;
    assert_provider_fail_closed(&unknown, "not-a-real-provider");
}
