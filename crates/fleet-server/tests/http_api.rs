//! Fail-closed HTTP integration tests for fleet-server (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use fleet_auth::{AuthConfig, ACCESS_COOKIE, REFRESH_COOKIE};
use fleet_db::{connect_and_migrate_with, PoolConfig};
use fleet_integrations::{
    credentials_for, issue_oauth_state, verify_oauth_state, CredentialsKey, IntegrationsConfig,
    OAuthClientConfig, OAuthStateClaims,
};
use fleet_orchestrator::api::build_app_state;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::{
    build_http_app, build_router, AppState, ClaudeConfig, GrokConfig, OllamaConfig, OpenaiConfig,
    ServerConfig,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_PASSWORD: &str = "correct-horse";
const TEST_ADMIN_TOKEN: &str = "test-admin";
const TEST_JWT_SECRET: &str = "dev-change-me-to-a-long-random-secret-at-least-32-chars";
/// Dead pool for `health_db_down_*` — not the suite DATABASE_URL (port 1 / closed).
const UNACQUIRABLE_DB_URL: &str =
    "postgres://health_db_down:health-db-down-secret-not-for-body@127.0.0.1:1/none";
const UNACQUIRABLE_DB_PASSWORD: &str = "health-db-down-secret-not-for-body";

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
    app_with_state_admin_token(TEST_ADMIN_TOKEN).await
}

/// Clone of [`app_with_state`] with a chosen ADMIN_TOKEN. AuthMode stays Token
/// — `AuthMode::None` skips AdminAuth and is not the unset-token lock.
async fn app_with_state_admin_token(admin_token: &str) -> (axum::Router, AppState) {
    app_with_state_parts(admin_token, None, None).await
}

/// Clone of [`app_with_state`] with a chosen integrations config (OAuth clients).
async fn app_with_integrations(integrations: IntegrationsConfig) -> (axum::Router, AppState) {
    app_with_state_parts(TEST_ADMIN_TOKEN, Some(integrations), None).await
}

/// Clone of [`app_with_state`] with a chosen `CORS_ORIGINS` list. Default
/// fixture (localhost + 127.0.0.1:3030) stays the list-cors-unknown-origin
/// allowlist — do not pass `["*"]` through that path.
async fn app_with_cors_origins(cors_origins: Vec<String>) -> axum::Router {
    app_with_state_parts(TEST_ADMIN_TOKEN, None, Some(cors_origins))
        .await
        .0
}

async fn app_with_state_parts(
    admin_token: &str,
    integrations: Option<IntegrationsConfig>,
    cors_origins: Option<Vec<String>>,
) -> (axum::Router, AppState) {
    let database_url = require_database_url();

    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);

    let orch_cfg = OrchConfig {
        database_url: database_url.clone(),
        admin_token: admin_token.to_string(),
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
    let integrations = integrations.unwrap_or_else(|| test_integrations_config(&jwt_secret));
    let config = ServerConfig {
        database_url: database_url.clone(),
        bind: "127.0.0.1:0".parse().expect("bind"),
        cors_origins: cors_origins.unwrap_or_else(|| {
            vec![
                "http://localhost:3030".into(),
                "http://127.0.0.1:3030".into(),
            ]
        }),
        auth,
        integrations,
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

async fn app() -> axum::Router {
    app_with_state().await.0
}

/// Closed lazy pool on a URL that is not the suite DATABASE_URL.
/// /health acquires `AppState.db`; /readyz acquires the orch store pool.
async fn app_with_unacquirable_db() -> axum::Router {
    let db = fleet_db::PgPool::connect_lazy(UNACQUIRABLE_DB_URL)
        .expect("lazy pool that never talks to the suite DATABASE_URL");
    db.close().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);

    let store = fleet_orchestrator::store::Store::new(db.clone());
    let runtime = std::sync::Arc::new(fleet_orchestrator::sandbox::StubRuntime::new(
        Duration::from_millis(1),
    ));
    let tick = std::sync::Arc::new(fleet_orchestrator::orch::TickRunner::new(
        store.clone(),
        runtime,
        data_dir,
        "http://127.0.0.1:0",
    ));
    let orch = fleet_orchestrator::api::AppState {
        store,
        tick,
        admin_token: TEST_ADMIN_TOKEN.into(),
        auth_mode: AuthMode::Token,
    };

    let auth = test_auth_config();
    let jwt_secret = auth.jwt_secret.clone();
    let integrations = test_integrations_config(&jwt_secret);
    let config = ServerConfig {
        database_url: UNACQUIRABLE_DB_URL.into(),
        bind: "127.0.0.1:0".parse().expect("bind"),
        cors_origins: vec![
            "http://localhost:3030".into(),
            "http://127.0.0.1:3030".into(),
        ],
        auth,
        integrations,
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
    build_http_app(state, orch, &config).expect("build_http_app")
}

/// JWT shaped like OAuth state but with `typ: "access"` (must be rejected by callback).
fn oauth_state_with_typ_access(secret: &str, provider: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};
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

/// Signed with a secret the server does not know — looks like state, must not verify.
fn forged_oauth_state(user_id: Uuid, workspace_id: Uuid, provider: &str) -> String {
    issue_oauth_state(
        "forged-not-the-server-jwt-secret-at-least-32-chars",
        user_id,
        workspace_id,
        provider,
    )
    .expect("encode forged oauth state")
}

fn expired_oauth_state(secret: &str, user_id: Uuid, workspace_id: Uuid, provider: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};

    let now = chrono::Utc::now().timestamp();
    encode(
        &Header::default(),
        &OAuthStateClaims {
            sub: user_id.to_string(),
            workspace_id: workspace_id.to_string(),
            provider: provider.to_string(),
            nonce: Uuid::now_v7().to_string(),
            exp: now - 120,
            iat: now - 180,
            typ: "oauth_state".into(),
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("encode expired oauth state")
}

fn github_manifest_callback_uri(code: Option<&str>, state: Option<&str>) -> String {
    const PATH: &str = "/api/integrations/oauth/github/manifest/callback";
    match (code, state) {
        (None, None) => PATH.to_string(),
        (Some(code), None) => format!("{PATH}?code={code}"),
        (None, Some(state)) => format!("{PATH}?state={state}"),
        (Some(code), Some(state)) => format!("{PATH}?code={code}&state={state}"),
    }
}

/// Manifest callback is public; reject must not 5xx, leak secrets, or 302 to install.
fn assert_github_manifest_callback_closed(out: &HttpOut, label: &str) {
    assert!(
        !out.status.is_server_error(),
        "{label} must not 5xx: {} {}",
        out.status,
        out.raw
    );
    let loc = response_location(out);
    let loc_l = loc.to_ascii_lowercase();
    assert_ne!(
        out.status,
        StatusCode::FOUND,
        "{label} must not 302 to persist/install: {} loc={loc}",
        out.status
    );
    assert_ne!(
        out.status,
        StatusCode::TEMPORARY_REDIRECT,
        "{label} must not 307 to persist/install: {} loc={loc}",
        out.status
    );
    assert!(
        loc.is_empty()
            || !(loc_l.contains("github.com/apps/") && loc_l.contains("/installations/new")),
        "{label} must not open github.com as a success path that upserts: {loc}"
    );
    let client_err = matches!(
        out.status,
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
    );
    let popup_html = out.raw.to_ascii_lowercase().contains("<html");
    assert!(
        client_err || popup_html,
        "{label} must be 400/401/404 or popup-close HTML, got {} {}",
        out.status,
        out.raw
    );
    let hay = format!("{} {} {}", out.raw, out.body, loc);
    for needle in [
        "client_secret",
        "webhook_secret",
        "pem",
        "access_token",
        "refresh_token",
    ] {
        assert!(
            !hay.contains(needle),
            "{label} must not leak {needle}: {}",
            out.raw
        );
    }
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
    extra_headers: &[(&str, &str)],
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
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
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
    call_inner(app, method, uri, cookies, None, &[], body).await
}

/// Bootstrap `POST …/oauth/start` is `application/x-www-form-urlencoded`, not JSON.
async fn call_form(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    form: &str,
) -> HttpOut {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(jar) = cookies {
        if let Some(h) = jar.cookie_header() {
            builder = builder.header(header::COOKIE, h);
        }
    }
    let req = builder.body(Body::from(form.to_string())).unwrap();
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
    call_inner(app, method, uri, None, Some(bearer), &[], body).await
}

async fn call_headers(
    app: &axum::Router,
    method: &str,
    uri: &str,
    extra_headers: &[(&str, &str)],
    body: Option<Value>,
) -> HttpOut {
    call_inner(app, method, uri, None, None, extra_headers, body).await
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
    let paired = call(
        app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": token,
            "hostname": "test-box",
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

async fn drain_leases(app: &axum::Router, machine: &str) {
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
        drain_leases(app, &machine).await;
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
    for need in [
        "auth",
        "workspaces",
        "personas",
        "integrations",
        "chat",
        "fleet",
    ] {
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

fn postgres_url_userinfo(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))?;
    let userinfo = rest.split_once('@')?.0;
    if userinfo.contains(':') && !userinfo.is_empty() {
        Some(userinfo)
    } else {
        None
    }
}

fn postgres_url_password(url: &str) -> Option<&str> {
    postgres_url_userinfo(url)?
        .split_once(':')
        .map(|(_, password)| password)
        .filter(|password| !password.is_empty())
}

/// Docs §14: GET /health is 200 `{status: degraded, db: down}` when acquire
/// fails — not /readyz 503, not liveness 5xx, and the body leaks no secrets.
#[tokio::test]
async fn health_db_down_returns_200_degraded_and_leaks_no_secrets() {
    let fixture_db = require_database_url();
    assert_ne!(
        UNACQUIRABLE_DB_URL, fixture_db,
        "dead-pool URL must not be the suite DATABASE_URL"
    );

    let down = app_with_unacquirable_db().await;

    let health = call(&down, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must be 200 (not 500, not 503) when DB acquire fails: {}",
        health.raw
    );
    assert_ne!(
        health.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "GET /health must not 500 when acquire fails: {}",
        health.raw
    );
    assert_ne!(
        health.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "GET /health is not /readyz — must not 503 when acquire fails: {}",
        health.raw
    );
    assert_eq!(
        health.body["status"], "degraded",
        "status must be degraded when acquire fails: {}",
        health.body
    );
    assert_ne!(
        health.body["status"], "ok",
        "must not report status=ok when the pool cannot acquire: {}",
        health.body
    );
    assert_eq!(
        health.body["db"], "down",
        "db must be down when acquire fails: {}",
        health.body
    );
    assert_ne!(
        health.body["db"], "up",
        "must not report db=up when the pool cannot acquire: {}",
        health.body
    );
    assert_eq!(
        health.body["service"], "fleet-server",
        "service must stay fleet-server: {}",
        health.body
    );
    let apis = health.body["apis"]
        .as_array()
        .expect("health.apis")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    for need in ["auth", "fleet", "chat"] {
        assert!(
            apis.contains(&need),
            "health.apis must still advertise {need} when db is down: {apis:?}"
        );
    }
    assert!(
        health.body.get("git_sha").is_some(),
        "health must expose git_sha (null when FLEET_GIT_SHA unset at compile): {}",
        health.body
    );

    let mut secrets: Vec<String> = vec![
        fixture_db.clone(),
        TEST_JWT_SECRET.to_string(),
        TEST_ADMIN_TOKEN.to_string(),
        UNACQUIRABLE_DB_URL.to_string(),
        UNACQUIRABLE_DB_PASSWORD.to_string(),
        "client_secret".into(),
        "access_token".into(),
    ];
    for key in [
        "DATABASE_URL",
        "JWT_SECRET",
        "ADMIN_TOKEN",
        "TEST_ADMIN_TOKEN",
    ] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                secrets.push(v);
            }
        }
    }
    if let Some(userinfo) = postgres_url_userinfo(&fixture_db) {
        secrets.push(userinfo.to_string());
        secrets.push(format!("://{userinfo}@"));
        if let Some(password) = postgres_url_password(&fixture_db) {
            let public = format!("{}{}", health.body["service"], health.body["apis"]);
            if !password.is_empty() && !public.contains(password) {
                secrets.push(password.to_string());
            }
        }
    }
    if let Some(userinfo) = postgres_url_userinfo(UNACQUIRABLE_DB_URL) {
        secrets.push(userinfo.to_string());
    }
    secrets.sort();
    secrets.dedup();
    for secret in &secrets {
        assert!(
            !secret.is_empty() && !health.raw.contains(secret),
            "GET /health raw body must not contain {secret:?}: {}",
            health.raw
        );
    }

    let healthz = call(&down, "GET", "/healthz", None, None).await;
    assert_eq!(
        healthz.status,
        StatusCode::OK,
        "GET /healthz must stay 200 (orch liveness does not acquire DB): {}",
        healthz.raw
    );
    assert_eq!(healthz.body["status"], "ok", "{}", healthz.body);

    let readyz = call(&down, "GET", "/readyz", None, None).await;
    assert_eq!(
        readyz.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "GET /readyz must stay 503 when acquire fails (do not weaken to 200): {}",
        readyz.raw
    );
    assert_ne!(
        readyz.status,
        StatusCode::OK,
        "GET /readyz must not 200 ready when the pool cannot acquire: {}",
        readyz.raw
    );

    let root = call(&down, "GET", "/", None, None).await;
    assert_eq!(
        root.status,
        StatusCode::OK,
        "GET / must stay 200: {}",
        root.raw
    );

    let live = app().await;
    let control = call(&live, "GET", "/health", None, None).await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "control GET /health on a live pool must stay 200: {}",
        control.raw
    );
    assert_eq!(
        control.body["status"], "ok",
        "control must be status=ok on a live pool (not every /health is degraded): {}",
        control.body
    );
    assert_eq!(
        control.body["db"], "up",
        "control must be db=up on a live pool: {}",
        control.body
    );
}

fn cors_header<'a>(out: &'a HttpOut, name: &str) -> Option<&'a str> {
    out.headers.get(name).and_then(|v| v.to_str().ok())
}

fn cors_acao(out: &HttpOut) -> Option<&str> {
    cors_header(out, header::ACCESS_CONTROL_ALLOW_ORIGIN.as_str())
}

fn cors_acac(out: &HttpOut) -> Option<&str> {
    cors_header(out, header::ACCESS_CONTROL_ALLOW_CREDENTIALS.as_str())
}

fn cors_acac_true(out: &HttpOut) -> bool {
    cors_acac(out).is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// `CORS_ORIGINS=*` is dropped (`AllowOrigin::list` after filtering `*`).
/// Must not reflect Origin, send `ACAO: *` (even without credentials), or
/// emit any ACAO for an unknown / star Origin. Listed-origin ACAO stays on
/// the default-fixture control (`assert_cors_listed_origin_with_credentials`).
fn assert_cors_star_closed(out: &HttpOut, origin: &str, label: &str) {
    let acao = cors_acao(out);
    assert!(
        acao.is_none(),
        "{label}: CORS_ORIGINS=* / unknown Origin must not send \
         Access-Control-Allow-Origin (got {acao:?}): {:?}",
        out.headers
    );
    assert_ne!(
        acao,
        Some(origin),
        "{label}: must not reflect Origin {origin} as Access-Control-Allow-Origin: {:?}",
        out.headers
    );
    assert_ne!(
        acao,
        Some("*"),
        "{label}: must not send Access-Control-Allow-Origin: * \
         (AllowOrigin::any() without credentials would still leak): {:?}",
        out.headers
    );
    assert!(
        !(acao == Some("*") && cors_acac_true(out)),
        "{label}: must not send Access-Control-Allow-Origin: * while \
         Access-Control-Allow-Credentials is true: {:?}",
        out.headers
    );
}

fn assert_set_cookie_not_readable_via_acao(out: &HttpOut, origin: &str, label: &str) {
    assert_cors_star_closed(out, origin, label);
    let minted = out.cookie(ACCESS_COOKIE).is_some() || out.cookie(REFRESH_COOKIE).is_some();
    if minted {
        assert_ne!(
            cors_acao(out),
            Some(origin),
            "{label}: Set-Cookie must not be readable by {origin} via ACAO: {:?}",
            parse_set_cookies(&out.headers)
        );
        assert!(
            !(cors_acao(out) == Some("*") && cors_acac_true(out)),
            "{label}: Set-Cookie must not be readable via ACAO * + credentials: {:?}",
            parse_set_cookies(&out.headers)
        );
    }
}

/// Listed UI origin must echo as ACAO with credentials (browser `credentials: include`).
fn assert_cors_listed_origin_with_credentials(out: &HttpOut, origin: &str, label: &str) {
    assert_eq!(
        cors_acao(out),
        Some(origin),
        "{label}: must include Access-Control-Allow-Origin: {origin}: {:?}",
        out.headers
    );
    assert!(
        cors_acac_true(out),
        "{label}: must include Access-Control-Allow-Credentials: true: {:?}",
        out.headers
    );
}

fn assert_ok_or_no_content(out: &HttpOut, label: &str) {
    assert!(
        out.status == StatusCode::OK || out.status == StatusCode::NO_CONTENT,
        "{label}: expected 200/204, got {}: {}",
        out.status,
        out.body
    );
}

/// Default fixture `CORS_ORIGINS` (localhost + 127.0.0.1:3030): unknown Origin
/// gets no ACAO echo / no `*` + credentials; listed UI origin gets both.
#[tokio::test]
async fn cors_unknown_origin_no_acao_listed_gets_credentials() {
    const EVIL: &str = "http://evil.example.test";
    const LISTED: &str = "http://localhost:3030";
    const LISTED_LOOPBACK: &str = "http://127.0.0.1:3030";
    let app = app().await;

    let health_evil = call_headers(&app, "GET", "/health", &[("origin", EVIL)], None).await;
    assert_eq!(
        health_evil.status,
        StatusCode::OK,
        "GET /health from {EVIL}: {}",
        health_evil.body
    );
    assert_cors_star_closed(
        &health_evil,
        EVIL,
        "GET /health default-fixture Origin evil",
    );

    let preflight_evil = call_headers(
        &app,
        "OPTIONS",
        "/api/auth/login",
        &[
            ("origin", EVIL),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "content-type"),
        ],
        None,
    )
    .await;
    assert_cors_star_closed(
        &preflight_evil,
        EVIL,
        "OPTIONS /api/auth/login default-fixture Origin evil",
    );

    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "signup: {}",
        created.body
    );
    let login_evil = call_headers(
        &app,
        "POST",
        "/api/auth/login",
        &[("origin", EVIL)],
        Some(json!({ "email": email, "password": TEST_PASSWORD })),
    )
    .await;
    assert_eq!(
        login_evil.status,
        StatusCode::OK,
        "login POST from {EVIL} (CORS closed, handler still 200): {}",
        login_evil.body
    );
    assert_set_cookie_not_readable_via_acao(
        &login_evil,
        EVIL,
        "login POST default-fixture Origin evil",
    );

    let health_listed = call_headers(&app, "GET", "/health", &[("origin", LISTED)], None).await;
    assert_ok_or_no_content(&health_listed, &format!("GET /health from {LISTED}"));
    assert_eq!(
        health_listed.status,
        StatusCode::OK,
        "GET /health from {LISTED}: {}",
        health_listed.body
    );
    assert_cors_listed_origin_with_credentials(
        &health_listed,
        LISTED,
        "GET /health default-fixture Origin listed",
    );

    let preflight_listed = call_headers(
        &app,
        "OPTIONS",
        "/api/auth/login",
        &[
            ("origin", LISTED),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "content-type"),
        ],
        None,
    )
    .await;
    assert_ok_or_no_content(
        &preflight_listed,
        &format!("OPTIONS /api/auth/login from {LISTED}"),
    );
    assert_cors_listed_origin_with_credentials(
        &preflight_listed,
        LISTED,
        "OPTIONS /api/auth/login default-fixture Origin listed",
    );

    let login_listed = call_headers(
        &app,
        "POST",
        "/api/auth/login",
        &[("origin", LISTED)],
        Some(json!({ "email": email, "password": TEST_PASSWORD })),
    )
    .await;
    assert_ok_or_no_content(&login_listed, &format!("login POST from {LISTED}"));
    assert_eq!(
        login_listed.status,
        StatusCode::OK,
        "login POST from {LISTED}: {}",
        login_listed.body
    );
    assert_cors_listed_origin_with_credentials(
        &login_listed,
        LISTED,
        "login POST default-fixture Origin listed",
    );
    assert!(
        login_listed.cookie(ACCESS_COOKIE).is_some()
            || login_listed.cookie(REFRESH_COOKIE).is_some(),
        "listed UI origin must still receive Set-Cookie with ACAO {LISTED}: {:?}",
        parse_set_cookies(&login_listed.headers)
    );

    let health_loopback =
        call_headers(&app, "GET", "/health", &[("origin", LISTED_LOOPBACK)], None).await;
    assert_ok_or_no_content(
        &health_loopback,
        &format!("GET /health from {LISTED_LOOPBACK}"),
    );
    assert_cors_listed_origin_with_credentials(
        &health_loopback,
        LISTED_LOOPBACK,
        "GET /health default-fixture Origin 127.0.0.1:3030",
    );
    let preflight_loopback = call_headers(
        &app,
        "OPTIONS",
        "/api/auth/login",
        &[
            ("origin", LISTED_LOOPBACK),
            ("access-control-request-method", "POST"),
        ],
        None,
    )
    .await;
    assert_ok_or_no_content(
        &preflight_loopback,
        &format!("OPTIONS /api/auth/login from {LISTED_LOOPBACK}"),
    );
    assert_cors_listed_origin_with_credentials(
        &preflight_loopback,
        LISTED_LOOPBACK,
        "OPTIONS /api/auth/login default-fixture Origin 127.0.0.1:3030",
    );

    let no_origin = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        no_origin.status,
        StatusCode::OK,
        "GET /health with no Origin still 200s: {}",
        no_origin.body
    );
    assert_eq!(no_origin.body["status"], "ok");
    assert_eq!(no_origin.body["service"], "fleet-server");
}

/// `CORS_ORIGINS=*` is a literal, not `AllowOrigin::any()` / reflect-every-Origin.
/// Default fixture still ACAO localhost:3030 with credentials (other card).
#[tokio::test]
async fn cors_origins_star_not_any_or_reflect() {
    const EVIL: &str = "http://evil.example.test";
    const LISTED: &str = "http://localhost:3030";
    let star = app_with_cors_origins(vec!["*".into()]).await;

    let health_evil = call_headers(&star, "GET", "/health", &[("origin", EVIL)], None).await;
    assert_eq!(
        health_evil.status,
        StatusCode::OK,
        "GET /health from {EVIL} on CORS_ORIGINS=*: {}",
        health_evil.body
    );
    assert_cors_star_closed(&health_evil, EVIL, "GET /health CORS_ORIGINS=* Origin evil");

    let preflight_evil = call_headers(
        &star,
        "OPTIONS",
        "/api/auth/login",
        &[
            ("origin", EVIL),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "content-type"),
        ],
        None,
    )
    .await;
    assert_cors_star_closed(
        &preflight_evil,
        EVIL,
        "OPTIONS /api/auth/login CORS_ORIGINS=* Origin evil",
    );

    let email = unique_email();
    let created = signup(&star, &email, TEST_PASSWORD).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "signup on * app: {}",
        created.body
    );
    let login_evil = call_headers(
        &star,
        "POST",
        "/api/auth/login",
        &[("origin", EVIL)],
        Some(json!({ "email": email, "password": TEST_PASSWORD })),
    )
    .await;
    assert_eq!(
        login_evil.status,
        StatusCode::OK,
        "login POST from {EVIL} on CORS_ORIGINS=* (CORS closed, handler still 200): {}",
        login_evil.body
    );
    assert_set_cookie_not_readable_via_acao(
        &login_evil,
        EVIL,
        "login POST CORS_ORIGINS=* Origin evil",
    );

    let health_star = call_headers(&star, "GET", "/health", &[("origin", "*")], None).await;
    assert_eq!(
        health_star.status,
        StatusCode::OK,
        "GET /health Origin *: {}",
        health_star.body
    );
    assert_cors_star_closed(&health_star, "*", "GET /health CORS_ORIGINS=* Origin *");
    assert_ne!(
        cors_acao(&health_star),
        Some(EVIL),
        "Origin: * must not unlock a real Origin as ACAO: {:?}",
        health_star.headers
    );
    assert_ne!(
        cors_acao(&health_star),
        Some(LISTED),
        "Origin: * must not unlock the UI origin as ACAO: {:?}",
        health_star.headers
    );
    assert!(
        !cors_acac_true(&health_star) || cors_acao(&health_star) != Some("*"),
        "Origin: * must not unlock credentials via ACAO * + ACAC: {:?}",
        health_star.headers
    );

    let preflight_star = call_headers(
        &star,
        "OPTIONS",
        "/api/auth/login",
        &[("origin", "*"), ("access-control-request-method", "POST")],
        None,
    )
    .await;
    assert_cors_star_closed(
        &preflight_star,
        "*",
        "OPTIONS /api/auth/login CORS_ORIGINS=* Origin *",
    );
    assert!(
        !cors_acac_true(&preflight_star) || cors_acao(&preflight_star) != Some("*"),
        "Origin: * preflight must not unlock credentials: {:?}",
        preflight_star.headers
    );

    let no_origin = call(&star, "GET", "/health", None, None).await;
    assert_eq!(
        no_origin.status,
        StatusCode::OK,
        "GET /health with no Origin on CORS_ORIGINS=* still 200s: {}",
        no_origin.body
    );
    assert_eq!(no_origin.body["status"], "ok");

    // Control: default fixture (list-cors-unknown-origin-no-acao) still ACAO
    // the listed UI origin with credentials. This card is the * config.
    let listed = app().await;
    let health_listed = call_headers(&listed, "GET", "/health", &[("origin", LISTED)], None).await;
    assert_eq!(
        health_listed.status,
        StatusCode::OK,
        "default-fixture GET /health from {LISTED}: {}",
        health_listed.body
    );
    assert_eq!(
        cors_acao(&health_listed),
        Some(LISTED),
        "default fixture must still ACAO {LISTED}: {:?}",
        health_listed.headers
    );
    assert!(
        cors_acac_true(&health_listed),
        "default fixture must still send Access-Control-Allow-Credentials: true: {:?}",
        health_listed.headers
    );
    let preflight_listed = call_headers(
        &listed,
        "OPTIONS",
        "/api/auth/login",
        &[
            ("origin", LISTED),
            ("access-control-request-method", "POST"),
        ],
        None,
    )
    .await;
    assert_eq!(
        cors_acao(&preflight_listed),
        Some(LISTED),
        "default-fixture OPTIONS must still ACAO {LISTED}: {:?}",
        preflight_listed.headers
    );
    assert!(
        cors_acac_true(&preflight_listed),
        "default-fixture OPTIONS must still send credentials: {:?}",
        preflight_listed.headers
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
        list.iter()
            .any(|t| t["plan_task_id"].as_str() == Some("t1")),
        "expected plan_task_id t1: {}",
        tasks.body
    );
}

/// Docs §6.5 / §8.1: cookie `POST /runs/fixture` `{kind: diamond}` is
/// root → left/right → join. POST must 201 without spawning. First tick
/// materializes the four-task DAG (not a linear chain of 4). Unknown kind
/// is 400 fail-closed, not a silent chain. Filter `-- fixture`.
/// catalog:list-diamond-fixture-four-tasks
#[tokio::test]
async fn cookie_fixture_kind_diamond_inserts_four_task_dag_without_ticking() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let anon = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        None,
        Some(json!({ "kind": "diamond" })),
    )
    .await;
    assert_eq!(
        anon.status,
        StatusCode::UNAUTHORIZED,
        "unauthenticated POST /runs/fixture must stay 401: {}",
        anon.body
    );
    assert!(
        anon.body.get("run_id").is_none() || anon.body["run_id"].is_null(),
        "unauthenticated POST must not return a run_id: {}",
        anon.body
    );

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "diamond" })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "cookie POST /runs/fixture kind=diamond must 201: {}",
        fx.body
    );
    assert_eq!(fx.body["kind"], "diamond", "{}", fx.body);
    assert_eq!(fx.body["project_id"].as_str(), Some(ws.as_str()));
    let run_id = fx.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("diamond fixture run_id: {}", fx.body))
        .to_string();

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "GET /runs: {}", runs.body);
    let run_ids = fleet_run_ids(&runs.body);
    assert!(
        run_ids.iter().any(|id| id == &run_id),
        "GET /runs must list diamond run {run_id}: {}",
        runs.body
    );

    let before_tick = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        before_tick.status,
        StatusCode::OK,
        "GET /tasks after POST (no tick): {}",
        before_tick.body
    );
    let before_list = fleet_tasks_array(&before_tick.body);
    let spawned_before: Vec<&str> = before_list
        .iter()
        .filter(|t| t["run_id"].as_str() == Some(run_id.as_str()))
        .filter_map(|t| t["plan_task_id"].as_str())
        .collect();
    assert!(
        spawned_before.is_empty(),
        "POST /runs/fixture must not tick / materialize the four diamond tasks: {spawned_before:?} {}",
        before_tick.body
    );
    for id in ["root", "left", "right", "join"] {
        assert!(
            !before_list
                .iter()
                .any(|t| t["plan_task_id"].as_str() == Some(id)),
            "POST must not spawn plan_task_id {id}: {}",
            before_tick.body
        );
    }

    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "tick: {}", tick.body);

    let after_tick = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        after_tick.status,
        StatusCode::OK,
        "GET /tasks after tick: {}",
        after_tick.body
    );
    let diamond: Vec<&Value> = fleet_tasks_array(&after_tick.body)
        .iter()
        .copied()
        .filter(|t| t["run_id"].as_str() == Some(run_id.as_str()))
        .collect();
    let plan_ids: Vec<&str> = diamond
        .iter()
        .filter_map(|t| t["plan_task_id"].as_str())
        .collect();
    assert_eq!(
        diamond.len(),
        4,
        "first tick must materialize four diamond tasks, got {plan_ids:?}: {}",
        after_tick.body
    );
    for id in ["root", "left", "right", "join"] {
        assert!(
            plan_ids.contains(&id),
            "expected plan_task_id {id} in diamond run, got {plan_ids:?}: {}",
            after_tick.body
        );
    }
    assert!(
        !plan_ids.contains(&"t1") && !plan_ids.contains(&"t2"),
        "diamond must not materialize as a chain (t1/t2): {plan_ids:?} {}",
        after_tick.body
    );

    let root = fleet_task_by_plan(&after_tick.body, "root");
    let left = fleet_task_by_plan(&after_tick.body, "left");
    let right = fleet_task_by_plan(&after_tick.body, "right");
    let join = fleet_task_by_plan(&after_tick.body, "join");
    assert_eq!(root["title"].as_str(), Some("Root"), "{}", after_tick.body);
    assert_eq!(
        left["title"].as_str(),
        Some("Left branch"),
        "{}",
        after_tick.body
    );
    assert_eq!(
        right["title"].as_str(),
        Some("Right branch"),
        "{}",
        after_tick.body
    );
    assert_eq!(join["title"].as_str(), Some("Join"), "{}", after_tick.body);

    let root_id = task_id_str(root, "root");
    let left_id = task_id_str(left, "left");
    let right_id = task_id_str(right, "right");
    let root_deps = json_id_vec(&root["depends_on"], "root");
    let left_deps = json_id_vec(&left["depends_on"], "left");
    let right_deps = json_id_vec(&right["depends_on"], "right");
    let join_deps = json_id_vec(&join["depends_on"], "join");
    assert!(
        root_deps.is_empty(),
        "root must have no deps (not a linear chain): {root_deps:?}"
    );
    assert_eq!(
        left_deps,
        vec![root_id.to_string()],
        "left must depend only on root, not a chain through right: {left_deps:?}"
    );
    assert_eq!(
        right_deps,
        vec![root_id.to_string()],
        "right must depend only on root, not on left (that would be a linear chain): {right_deps:?}"
    );
    let mut join_sorted = join_deps.clone();
    join_sorted.sort();
    let mut expected_join = vec![left_id.to_string(), right_id.to_string()];
    expected_join.sort();
    assert_eq!(
        join_sorted, expected_join,
        "join must depend on left and right, not a single parent chain: {join_deps:?}"
    );

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "GET /runs snapshot: {}",
        snap.body
    );
    let snap_ids = fleet_run_ids(&snap.body);
    let unknown = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "not-a-real-kind" })),
    )
    .await;
    assert_ne!(
        unknown.status,
        StatusCode::CREATED,
        "unknown fixture kind must not 201 as a silent chain: {}",
        unknown.body
    );
    assert_eq!(
        unknown.status,
        StatusCode::BAD_REQUEST,
        "unknown fixture kind must 400 fail-closed, not {}: {}",
        unknown.status,
        unknown.body
    );
    assert!(
        unknown.body.get("run_id").is_none() || unknown.body["run_id"].is_null(),
        "unknown kind 400 must not return a run_id: {}",
        unknown.body
    );
    assert_ne!(
        unknown.body.get("kind"),
        Some(&json!("chain")),
        "unknown kind must not silently become chain: {}",
        unknown.body
    );
    let err = unknown.body["error"].as_str().unwrap_or("");
    assert!(
        !err.is_empty(),
        "unknown kind 400 must name the error: {}",
        unknown.body
    );

    let after_unknown = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_unknown.status,
        StatusCode::OK,
        "GET /runs after unknown kind: {}",
        after_unknown.body
    );
    let after_unknown_ids = fleet_run_ids(&after_unknown.body);
    assert_eq!(
        after_unknown_ids, snap_ids,
        "GET /runs must not grow a new run from the 400: snap={snap_ids:?} after={after_unknown_ids:?} {}",
        after_unknown.body
    );

    let chain = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        chain.status,
        StatusCode::CREATED,
        "control POST kind=chain must still 201: {}",
        chain.body
    );
    assert_eq!(chain.body["kind"], "chain", "{}", chain.body);
    assert!(
        chain.body["run_id"].as_str().is_some(),
        "chain fixture run_id: {}",
        chain.body
    );
}

fn fleet_tasks_array(body: &Value) -> Vec<&Value> {
    body["tasks"].as_array().into_iter().flatten().collect()
}

fn task_id_str<'a>(task: &'a Value, plan_task_id: &str) -> &'a str {
    task["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{plan_task_id} task id: {task}"))
}

fn json_id_vec(value: &Value, plan_task_id: &str) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{plan_task_id} depends_on must be an array: {value}"))
        .iter()
        .map(|d| {
            d.as_str()
                .unwrap_or_else(|| panic!("{plan_task_id} depends_on entry: {d}"))
                .to_string()
        })
        .collect()
}

/// Docs: POST /fleet/tick and /v1/.../admin/tick are 409 if the project
/// advisory lock is held. A second cookie or admin tick must not 200 with
/// spawned=0 (that is a swallowed StoreError::Conflict as Ok(empty TickReport)).
#[tokio::test]
async fn tick_while_project_lock_held_returns_409_not_empty_ok() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let project_id = Uuid::parse_str(&ws).expect("workspace id");

    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);

    let mut lock_conn = state
        .orch
        .store
        .lock_project(project_id)
        .await
        .expect("hold project advisory lock");

    let cookie_tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_tick_lock_held_409(&cookie_tick, "cookie POST /fleet/tick");

    let admin_tick = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/admin/tick"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_tick_lock_held_409(&admin_tick, "admin POST /v1/.../admin/tick");

    fleet_orchestrator::store::Store::unlock_project(&mut lock_conn, project_id)
        .await
        .expect("release project advisory lock");

    // Control: after unlock, cookie tick is 200 and actually works — not a
    // tautology that every tick is 409.
    let after = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "tick after unlock: {}",
        after.body
    );
    let spawned = after.body["spawned"].as_u64().unwrap_or(0);
    let materialized = after.body["tasks_materialized"].as_u64().unwrap_or(0);
    let plans = after.body["plans_materialized"].as_u64().unwrap_or(0);
    assert!(
        spawned >= 1 || materialized >= 1 || plans >= 1,
        "tick after unlock must do work, not look like a swallowed empty report: {}",
        after.body
    );
}

fn assert_tick_lock_held_409(out: &HttpOut, label: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not 200 with a silent empty report: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::CONFLICT,
        "{label} must be 409 while the project lock is held, not {}: {}",
        out.status,
        out.body
    );
    let err = out.body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        !err.is_empty(),
        "{label} 409 body must be an error, not a TickReport: {}",
        out.body
    );
    let lc = err.to_ascii_lowercase();
    assert!(
        lc.contains("lock") || lc.contains("already running") || lc.contains("tick_lock_held"),
        "{label} error must name the held tick lock: {err:?} body={}",
        out.body
    );
    assert!(
        out.body.get("spawned").is_none(),
        "{label} must not return a TickReport (spawned) while lock is held: {}",
        out.body
    );
}

#[tokio::test]
async fn resume_non_blocked_task_is_conflict_and_unknown_kill_is_404() {
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
    let _ = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;

    let tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    let task_id = tasks.body["tasks"][0]["id"].as_str().expect("task id");
    let resume = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/tasks/{task_id}/resume")),
        Some(&jar),
        Some(json!({ "message": "go" })),
    )
    .await;
    assert_eq!(resume.status, StatusCode::CONFLICT, "{}", resume.body);

    let missing = uuid::Uuid::new_v4();
    let kill = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/sessions/{missing}/kill")),
        Some(&jar),
        Some(json!({ "reason": "test" })),
    )
    .await;
    assert_eq!(kill.status, StatusCode::NOT_FOUND, "{}", kill.body);
}

/// Cookie BFF: B's member cookie on **B's** workspace must not resume A's
/// blocked `task_id`. Product SQL is `id AND project_id AND status='blocked'`
/// — a missing `project_id` clause would set `hitl_resume_requested_at` on A
/// from B, then A's next tick would promote. 404 or 409 "task is not blocked"
/// are ok (409 is the same status as own non-blocked); the lock is A's row
/// unchanged. B on A's path stays 403. A's own non-blocked stays 409; A's
/// own blocked may 200 so this is not a tautology that every resume 409s.
#[tokio::test]
async fn cookie_resume_foreign_task_id_does_not_clear_blocked() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    let a_project = Uuid::parse_str(&a_ws).expect("A workspace id");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    // Materialize without a spawn tick: stub_duration=0 finishes t1 in the
    // same HTTP tick (done_success → blocked is illegal). Store-walk t1 to
    // blocked; t2 stays queued for the own-non-blocked 409 control.
    let plan_id =
        Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan uuid");
    let n = state
        .orch
        .store
        .materialize_plan(a_project, plan_id)
        .await
        .expect("materialize fixture");
    assert!(n >= 2, "chain fixture must materialize t1+t2, got {n}");

    let listed = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let t1 = fleet_task_by_plan(&listed.body, "t1");
    let t2 = fleet_task_by_plan(&listed.body, "t2");
    let t1_id = t1["id"].as_str().expect("t1 id").to_string();
    let t2_id = t2["id"].as_str().expect("t2 id").to_string();
    let t1_uuid = Uuid::parse_str(&t1_id).expect("t1 uuid");
    let t1_from = t1["status"].as_str().expect("t1 status").to_string();
    assert_ne!(
        t1_from, "blocked",
        "fresh fixture must not already block t1: {}",
        listed.body
    );
    assert_eq!(
        t2["status"].as_str(),
        Some("queued"),
        "t2 stays non-blocked for the 409 control: {}",
        listed.body
    );

    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "queued", "ready", None)
            .await
            .expect("t1 queued → ready"),
        "setup must CAS t1 queued → ready"
    );
    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "ready", "assigned", None)
            .await
            .expect("t1 ready → assigned"),
        "setup must CAS t1 ready → assigned"
    );
    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "assigned", "blocked", Some("needs_approval"))
            .await
            .expect("t1 assigned → blocked"),
        "setup must CAS t1 assigned → blocked so resume has a HITL task"
    );

    let snap = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "{}", snap.body);
    let t1_snap = fleet_task_by_plan(&snap.body, "t1");
    assert_eq!(
        t1_snap["status"].as_str(),
        Some("blocked"),
        "member GET must see blocked before foreign resume: {}",
        snap.body
    );
    assert!(
        t1_snap["hitl_resume_requested_at"].is_null(),
        "GET snapshot hitl_resume_requested_at must be null: {}",
        snap.body
    );
    let notes_snap = t1_snap["hitl_notes"].clone();
    let stored_snap = state
        .orch
        .store
        .get_task(a_project, t1_uuid)
        .await
        .expect("get t1")
        .expect("t1 row");
    assert_eq!(stored_snap.status, "blocked");
    assert!(
        stored_snap.hitl_resume_requested_at.is_none(),
        "row snapshot hitl_resume_requested_at must be null: {:?}",
        stored_snap.hitl_resume_requested_at
    );

    let b_msg = format!("b-foreign-resume-{}", Uuid::new_v4());
    let stolen_on_b = call(
        &app,
        "POST",
        &fleet(&b_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&b_jar),
        Some(json!({ "message": b_msg })),
    )
    .await;
    assert_ne!(
        stolen_on_b.status,
        StatusCode::OK,
        "B's cookie on B's path must not 200 resume A's task: {}",
        stolen_on_b.body
    );
    assert_ne!(
        stolen_on_b.body.get("resume_requested"),
        Some(&json!(true)),
        "must not claim resume_requested: {}",
        stolen_on_b.body
    );
    assert!(
        stolen_on_b.status == StatusCode::NOT_FOUND || stolen_on_b.status == StatusCode::CONFLICT,
        "B on B's path with A's task_id must be 404 or 409, not {}: {}",
        stolen_on_b.status,
        stolen_on_b.body
    );
    if stolen_on_b.status == StatusCode::CONFLICT {
        let err = stolen_on_b.body["error"].as_str().unwrap_or("");
        assert!(
            err.to_ascii_lowercase().contains("not blocked"),
            "409 must say task is not blocked (same as own non-blocked): {}",
            stolen_on_b.body
        );
    }
    assert!(
        !stolen_on_b.raw.contains(&b_msg),
        "B's unique message must not echo on the reject: {}",
        stolen_on_b.raw
    );

    let after_b_path = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after_b_path.status, StatusCode::OK, "{}", after_b_path.body);
    let t1_after_b_path = fleet_task_by_plan(&after_b_path.body, "t1");
    assert_eq!(
        t1_after_b_path["status"].as_str(),
        Some("blocked"),
        "B's path + A's task_id must not clear blocked: {}",
        after_b_path.body
    );
    assert_eq!(
        t1_after_b_path["hitl_notes"], notes_snap,
        "B's path must not write hitl_notes: {}",
        after_b_path.body
    );
    let notes_str = t1_after_b_path["hitl_notes"].as_str().unwrap_or("");
    assert!(
        !notes_str.contains(&b_msg),
        "B's message must not stick: {}",
        after_b_path.body
    );
    let stored_after_b_path = state
        .orch
        .store
        .get_task(a_project, t1_uuid)
        .await
        .expect("get t1 after B path")
        .expect("t1 row");
    assert_eq!(stored_after_b_path.status, "blocked");
    assert!(
        stored_after_b_path.hitl_resume_requested_at.is_none(),
        "B's path must not set hitl_resume_requested_at: {:?}",
        stored_after_b_path.hitl_resume_requested_at
    );

    let later_tick = call(&app, "POST", &fleet(&a_ws, "/tick"), Some(&a_jar), None).await;
    assert_eq!(
        later_tick.status,
        StatusCode::OK,
        "A later tick: {}",
        later_tick.body
    );
    let after_tick = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after_tick.status, StatusCode::OK, "{}", after_tick.body);
    let t1_after_tick = fleet_task_by_plan(&after_tick.body, "t1");
    assert_eq!(
        t1_after_tick["status"].as_str(),
        Some("blocked"),
        "A later tick must not promote from B's message: {}",
        after_tick.body
    );
    let notes_after_tick = t1_after_tick["hitl_notes"].as_str().unwrap_or("");
    assert!(
        !notes_after_tick.contains(&b_msg),
        "A later tick must not apply B's message: {}",
        after_tick.body
    );
    let stored_after_tick = state
        .orch
        .store
        .get_task(a_project, t1_uuid)
        .await
        .expect("get t1 after A tick")
        .expect("t1 row");
    assert_eq!(stored_after_tick.status, "blocked");
    assert!(
        stored_after_tick.hitl_resume_requested_at.is_none(),
        "A later tick must not have seen a resume intent from B: {:?}",
        stored_after_tick.hitl_resume_requested_at
    );

    let stolen_on_a = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&b_jar),
        Some(json!({ "message": b_msg })),
    )
    .await;
    assert_eq!(
        stolen_on_a.status,
        StatusCode::FORBIDDEN,
        "B's cookie on A's path must 403: {}",
        stolen_on_a.body
    );
    let stolen_err = stolen_on_a.body["error"].as_str().unwrap_or("");
    assert!(
        stolen_err.to_ascii_lowercase().contains("not a member"),
        "403 must say not a member of this workspace: {}",
        stolen_on_a.body
    );
    assert_ne!(
        stolen_on_a.body.get("resume_requested"),
        Some(&json!(true)),
        "403 must not claim resume_requested: {}",
        stolen_on_a.body
    );
    let after_403 = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after_403.status, StatusCode::OK, "{}", after_403.body);
    let t1_after_403 = fleet_task_by_plan(&after_403.body, "t1");
    assert_eq!(
        t1_after_403["status"].as_str(),
        Some("blocked"),
        "403 must not clear blocked: {}",
        after_403.body
    );
    let stored_after_403 = state
        .orch
        .store
        .get_task(a_project, t1_uuid)
        .await
        .expect("get t1 after 403")
        .expect("t1 row");
    assert_eq!(stored_after_403.status, "blocked");
    assert!(
        stored_after_403.hitl_resume_requested_at.is_none(),
        "403 must not set hitl_resume_requested_at: {:?}",
        stored_after_403.hitl_resume_requested_at
    );

    let own_ready = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t2_id}/resume")),
        Some(&a_jar),
        Some(json!({ "message": "go" })),
    )
    .await;
    assert_eq!(
        own_ready.status,
        StatusCode::CONFLICT,
        "A's cookie on A's own non-blocked task still 409: {}",
        own_ready.body
    );
    let own_err = own_ready.body["error"].as_str().unwrap_or("");
    assert!(
        own_err.to_ascii_lowercase().contains("not blocked"),
        "own non-blocked 409 must say task is not blocked: {}",
        own_ready.body
    );

    let own_blocked = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&a_jar),
        Some(json!({ "message": "owner-resume" })),
    )
    .await;
    assert_eq!(
        own_blocked.status,
        StatusCode::OK,
        "A's cookie on A's own blocked task may 200 (not a tautology that every resume 409s): {}",
        own_blocked.body
    );
    assert_eq!(
        own_blocked.body["resume_requested"], true,
        "owner resume must request HITL: {}",
        own_blocked.body
    );
}

/// Cookie BFF HITL resume: a blocked task + workspace member cookie requests
/// resume and the in-handler tick promotes blocked → ready. Non-blocked stays
/// 409. Other-workspace cookie is 403 and must not clear blocked. Empty
/// message defaults (must not 500). Docs still say admin-only; the route is
/// `require_member`. Admin `/v1/.../resume` is unchanged (AdminAuth).
#[tokio::test]
async fn cookie_resume_blocked_task_promotes_to_ready_other_workspace_403() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let project_id = Uuid::parse_str(&a_ws).expect("workspace id");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    // Materialize without a spawn tick: stub_duration=0 finishes t1 in the
    // same HTTP tick (done_success → blocked is illegal). Store-walk t1 to
    // blocked; t2 stays queued for the own-non-blocked 409 control.
    let plan_id =
        Uuid::parse_str(fx.body["plan_id"].as_str().expect("plan_id")).expect("plan uuid");
    let n = state
        .orch
        .store
        .materialize_plan(project_id, plan_id)
        .await
        .expect("materialize fixture");
    assert!(n >= 2, "chain fixture must materialize t1+t2, got {n}");

    let listed = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let t1 = fleet_task_by_plan(&listed.body, "t1");
    let t2 = fleet_task_by_plan(&listed.body, "t2");
    let t1_id = t1["id"].as_str().expect("t1 id").to_string();
    let t2_id = t2["id"].as_str().expect("t2 id").to_string();
    let t1_uuid = Uuid::parse_str(&t1_id).expect("t1 uuid");
    let t1_from = t1["status"].as_str().expect("t1 status").to_string();
    assert_ne!(
        t1_from, "blocked",
        "fresh fixture must not already block t1: {}",
        listed.body
    );
    assert_eq!(
        t2["status"].as_str(),
        Some("queued"),
        "t2 stays non-blocked for the 409 control: {}",
        listed.body
    );

    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "queued", "ready", None)
            .await
            .expect("t1 queued → ready"),
        "setup must CAS t1 queued → ready"
    );
    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "ready", "assigned", None)
            .await
            .expect("t1 ready → assigned"),
        "setup must CAS t1 ready → assigned"
    );
    assert!(
        state
            .orch
            .store
            .update_task_status(t1_uuid, "assigned", "blocked", Some("needs_approval"))
            .await
            .expect("t1 assigned → blocked"),
        "setup must CAS t1 assigned → blocked so resume has a HITL task"
    );

    let blocked = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(blocked.status, StatusCode::OK, "{}", blocked.body);
    let t1_blocked = fleet_task_by_plan(&blocked.body, "t1");
    assert_eq!(
        t1_blocked["status"].as_str(),
        Some("blocked"),
        "member GET must see blocked before resume: {}",
        blocked.body
    );
    assert!(
        t1_blocked["hitl_resume_requested_at"].is_null(),
        "GET snapshot hitl_resume_requested_at must be null: {}",
        blocked.body
    );
    let notes_before = t1_blocked["hitl_notes"].clone();
    let stored_snap = state
        .orch
        .store
        .get_task(project_id, t1_uuid)
        .await
        .expect("get t1")
        .expect("t1 row");
    assert_eq!(stored_snap.status, "blocked");
    assert!(
        stored_snap.hitl_resume_requested_at.is_none(),
        "row snapshot hitl_resume_requested_at must be null: {:?}",
        stored_snap.hitl_resume_requested_at
    );

    let t2_empty = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t2_id}/resume")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_ne!(
        t2_empty.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "empty body on a non-blocked task must not 500: {}",
        t2_empty.body
    );
    assert_eq!(
        t2_empty.status,
        StatusCode::CONFLICT,
        "non-blocked stays 409 (empty body may default): {}",
        t2_empty.body
    );
    assert_ne!(
        t2_empty.body.get("resume_requested"),
        Some(&json!(true)),
        "409 must not claim resume_requested: {}",
        t2_empty.body
    );
    let t2_empty_err = t2_empty.body["error"].as_str().unwrap_or("");
    assert!(
        t2_empty_err.to_ascii_lowercase().contains("not blocked"),
        "non-blocked 409 must say task is not blocked: {}",
        t2_empty.body
    );

    let t2_blank = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t2_id}/resume")),
        Some(&a_jar),
        Some(json!({ "message": "" })),
    )
    .await;
    assert_ne!(
        t2_blank.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "empty message on a non-blocked task must not 500: {}",
        t2_blank.body
    );
    assert_eq!(
        t2_blank.status,
        StatusCode::CONFLICT,
        "non-blocked stays 409: {}",
        t2_blank.body
    );
    let t2_blank_err = t2_blank.body["error"].as_str().unwrap_or("");
    assert!(
        t2_blank_err.to_ascii_lowercase().contains("not blocked"),
        "empty-message 409 must say task is not blocked: {}",
        t2_blank.body
    );

    let b_msg = format!("b-must-not-resume-{}", Uuid::new_v4());
    let stolen = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&b_jar),
        Some(json!({ "message": b_msg })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::FORBIDDEN,
        "other-workspace cookie must 403: {}",
        stolen.body
    );
    let stolen_err = stolen.body["error"].as_str().unwrap_or("");
    assert!(
        stolen_err.to_ascii_lowercase().contains("not a member"),
        "403 must say not a member of this workspace: {}",
        stolen.body
    );
    assert_ne!(
        stolen.body.get("resume_requested"),
        Some(&json!(true)),
        "403 must not claim resume_requested: {}",
        stolen.body
    );
    assert!(
        !stolen.raw.contains(&b_msg),
        "403 must not echo the resume message: {}",
        stolen.raw
    );

    let after_b = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after_b.status, StatusCode::OK, "{}", after_b.body);
    let t1_after_b = fleet_task_by_plan(&after_b.body, "t1");
    assert_eq!(
        t1_after_b["status"].as_str(),
        Some("blocked"),
        "other-workspace resume must not clear blocked: {}",
        after_b.body
    );
    assert_eq!(
        t1_after_b["hitl_notes"], notes_before,
        "other-workspace resume must not write hitl_notes: {}",
        after_b.body
    );
    let notes_str = t1_after_b["hitl_notes"].as_str().unwrap_or("");
    assert!(
        !notes_str.contains(&b_msg),
        "B's message must not stick: {}",
        after_b.body
    );
    let stored_after_b = state
        .orch
        .store
        .get_task(project_id, t1_uuid)
        .await
        .expect("get t1")
        .expect("t1 row");
    assert_eq!(stored_after_b.status, "blocked");
    assert!(
        stored_after_b.hitl_resume_requested_at.is_none(),
        "403 must not set hitl_resume_requested_at: {:?}",
        stored_after_b.hitl_resume_requested_at
    );

    let later_tick = call(&app, "POST", &fleet(&a_ws, "/tick"), Some(&a_jar), None).await;
    assert_eq!(
        later_tick.status,
        StatusCode::OK,
        "A later tick after 403: {}",
        later_tick.body
    );
    let after_tick = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after_tick.status, StatusCode::OK, "{}", after_tick.body);
    let t1_after_tick = fleet_task_by_plan(&after_tick.body, "t1");
    assert_eq!(
        t1_after_tick["status"].as_str(),
        Some("blocked"),
        "A later tick must not promote from B's 403: {}",
        after_tick.body
    );
    let stored_after_tick = state
        .orch
        .store
        .get_task(project_id, t1_uuid)
        .await
        .expect("get t1 after A tick")
        .expect("t1 row");
    assert_eq!(stored_after_tick.status, "blocked");
    assert!(
        stored_after_tick.hitl_resume_requested_at.is_none(),
        "A later tick must not have seen a resume intent from B: {:?}",
        stored_after_tick.hitl_resume_requested_at
    );

    let empty = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_ne!(
        empty.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "empty body on a blocked task must not 500: {}",
        empty.body
    );
    assert_eq!(
        empty.status,
        StatusCode::OK,
        "member cookie resume on blocked must 200: {}",
        empty.body
    );
    assert_eq!(
        empty.body["resume_requested"], true,
        "member resume must request HITL: {}",
        empty.body
    );
    assert_eq!(
        empty.body["task_id"].as_str(),
        Some(t1_id.as_str()),
        "{}",
        empty.body
    );
    assert_eq!(
        empty.body["message"].as_str(),
        Some("resumed from UI"),
        "empty message must default: {}",
        empty.body
    );

    let after = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    let t1_after = fleet_task_by_plan(&after.body, "t1");
    let after_status = t1_after["status"].as_str().unwrap_or("");
    assert_ne!(
        after_status, "blocked",
        "member resume + tick must leave blocked: {}",
        after.body
    );
    assert!(
        matches!(
            after_status,
            "ready" | "assigned" | "in_progress" | "done_success"
        ),
        "HITL tick promotes blocked → ready (same tick may spawn): {} {}",
        after_status,
        after.body
    );
    assert_eq!(
        t1_after["hitl_notes"].as_str(),
        Some("resumed from UI"),
        "default notes must persist: {}",
        after.body
    );
    assert!(
        t1_after["block_reason"].is_null()
            || t1_after["block_reason"]
                .as_str()
                .map(str::is_empty)
                .unwrap_or(false),
        "apply_hitl_resume clears block_reason: {}",
        after.body
    );

    let stored = state
        .orch
        .store
        .get_task(project_id, t1_uuid)
        .await
        .expect("get t1 after resume")
        .expect("t1 row");
    assert_ne!(stored.status, "blocked");
    assert!(
        stored.hitl_resolved_at.is_some(),
        "tick must apply HITL resume (hitl_resolved_at): {stored:?}"
    );
    assert!(
        stored.hitl_resume_requested_at.is_none(),
        "applied resume clears hitl_resume_requested_at: {stored:?}"
    );

    let again = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/tasks/{t1_id}/resume")),
        Some(&a_jar),
        Some(json!({ "message": "go again" })),
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "non-blocked after promote stays 409: {}",
        again.body
    );
    let again_err = again.body["error"].as_str().unwrap_or("");
    assert!(
        again_err.to_ascii_lowercase().contains("not blocked"),
        "after-promote 409 must say task is not blocked: {}",
        again.body
    );
}

fn fleet_task_by_plan<'a>(body: &'a Value, plan_task_id: &str) -> &'a Value {
    body["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["plan_task_id"].as_str() == Some(plan_task_id))
        .unwrap_or_else(|| panic!("task {plan_task_id} missing: {body}"))
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

/// Cookie BFF cancel is membership- and project-scoped. Knowing A's `run_id`
/// is not enough: B's cookie on B's path must not mark A's run cancelled
/// (`request_cancel` UPDATEs `id` AND `project_id`); B on A's path is 403
/// not-a-member; a session cookie must not authenticate admin `/v1` cancel.
/// Distinct from [`cancel_run_reaches_cancelled`] (own run),
/// [`cookie_resume_foreign_task_id_does_not_clear_blocked`] (resume), and
/// kill-cross-workspace. Filter `-- cancel`.
#[tokio::test]
async fn cookie_cancel_foreign_run_id_does_not_mark_cancelled() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");

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

    let snap = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(&snap, &run_id, "A GET /runs after fixture");

    let b_fleet = call(&app, "GET", &fleet(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_fleet.status,
        StatusCode::OK,
        "B GET /fleet must prime B's project so this is not a missing-project tautology: {}",
        b_fleet.body
    );

    let stolen_on_b = call(
        &app,
        "POST",
        &fleet(&b_ws, &format!("/runs/{run_id}/cancel")),
        Some(&b_jar),
        Some(json!({ "reason": "b-foreign-cancel" })),
    )
    .await;
    assert_ne!(
        stolen_on_b.status,
        StatusCode::OK,
        "B's cookie on B's path must not 200 cancel A's run: {}",
        stolen_on_b.body
    );
    assert_ne!(
        stolen_on_b.body.get("cancel_requested"),
        Some(&json!(true)),
        "must not claim cancel_requested: {}",
        stolen_on_b.body
    );
    let stolen_err = http_error_text(&stolen_on_b).to_ascii_lowercase();
    assert!(
        stolen_on_b.status == StatusCode::NOT_FOUND
            || (stolen_on_b.status == StatusCode::BAD_REQUEST && stolen_err.contains("not found")),
        "B on B's path with A's run_id must be 404 or 400 not-found, not {}: {}",
        stolen_on_b.status,
        stolen_on_b.body
    );

    let after_b_path = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(
        &after_b_path,
        &run_id,
        "A GET /runs after B cancel on B's path",
    );

    let later_tick = call(&app, "POST", &fleet(&a_ws, "/tick"), Some(&a_jar), None).await;
    assert_eq!(
        later_tick.status,
        StatusCode::OK,
        "A later tick must still run (not drain-cancelled): {}",
        later_tick.body
    );
    let after_tick = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(&after_tick, &run_id, "A GET /runs after A tick");
    let tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(tasks.status, StatusCode::OK, "{}", tasks.body);
    let list = tasks.body["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        !list.is_empty(),
        "A later tick must still materialize the run (must not have been drain-cancelled): {}",
        tasks.body
    );
    assert!(
        list.iter()
            .any(|t| t["plan_task_id"].as_str() == Some("t1")),
        "expected plan_task_id t1 after A tick: {}",
        tasks.body
    );

    let stolen_on_a = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/runs/{run_id}/cancel")),
        Some(&b_jar),
        Some(json!({ "reason": "b-on-a-cancel" })),
    )
    .await;
    assert_eq!(
        stolen_on_a.status,
        StatusCode::FORBIDDEN,
        "B's cookie on A's path must 403: {}",
        stolen_on_a.body
    );
    let stolen_a_err = http_error_text(&stolen_on_a).to_ascii_lowercase();
    assert!(
        stolen_a_err.contains("not a member"),
        "403 must say not a member of this workspace: {}",
        stolen_on_a.body
    );
    assert_ne!(
        stolen_on_a.body.get("cancel_requested"),
        Some(&json!(true)),
        "403 must not claim cancel_requested: {}",
        stolen_on_a.body
    );
    let after_403 = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(&after_403, &run_id, "A GET /runs after B 403 on A's path");

    let v1_cookie = call(
        &app,
        "POST",
        &format!("/v1/projects/{a_ws}/runs/{run_id}/cancel"),
        Some(&a_jar),
        Some(json!({ "reason": "cookie-v1-cancel" })),
    )
    .await;
    assert_eq!(
        v1_cookie.status,
        StatusCode::UNAUTHORIZED,
        "session cookie must not authorize POST /v1/projects/{{id}}/runs/{{run}}/cancel: {}",
        v1_cookie.body
    );
    assert_ne!(
        v1_cookie.body.get("cancel_requested"),
        Some(&json!(true)),
        "cookie /v1 cancel must not claim cancel_requested: {}",
        v1_cookie.body
    );
    let after_v1 = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(&after_v1, &run_id, "A GET /runs after cookie /v1 cancel");

    let b_fx = call(
        &app,
        "POST",
        &fleet(&b_ws, "/runs/fixture"),
        Some(&b_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        b_fx.status,
        StatusCode::CREATED,
        "B own fixture (not a tautology that every cancel 404s): {}",
        b_fx.body
    );
    let b_run = b_fx.body["run_id"].as_str().expect("B run_id").to_string();
    assert_ne!(b_run, run_id, "B's run must not be A's run");
    let b_own = call(
        &app,
        "POST",
        &fleet(&b_ws, &format!("/runs/{b_run}/cancel")),
        Some(&b_jar),
        Some(json!({ "reason": "b-own-cancel" })),
    )
    .await;
    assert_eq!(
        b_own.status,
        StatusCode::OK,
        "B's cookie on B's own run must still 200 (not a tautology that every cancel 404s): {}",
        b_own.body
    );
    assert_eq!(
        b_own.body["cancel_requested"], true,
        "owner cancel must request cancel: {}",
        b_own.body
    );
    let a_still = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_a_run_uncancelled(
        &a_still,
        &run_id,
        "A's run stays uncancelled after B own cancel",
    );
}

fn assert_a_run_uncancelled(runs: &HttpOut, run_id: &str, label: &str) {
    assert_eq!(runs.status, StatusCode::OK, "{label}: {}", runs.body);
    let run = runs.body["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["id"].as_str() == Some(run_id))
        .unwrap_or_else(|| panic!("{label}: run {run_id} missing: {}", runs.body));
    assert!(
        run["cancel_requested_at"].is_null(),
        "{label}: cancel_requested_at must stay null: {run}"
    );
    let status = run["status"].as_str().unwrap_or("");
    assert_ne!(
        status, "cancelled",
        "{label}: status must not be cancelled: {run}"
    );
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
    assert_eq!(
        ref_only.status,
        StatusCode::BAD_REQUEST,
        "{}",
        ref_only.body
    );
}

/// Docs §6.9 / §8.1: cookie PATCH `/fleet/config` must not persist or echo
/// URL userinfo (`user:pass@` / `token:x-oauth-basic@`). 400, or 200 with
/// GET `/fleet` `config.repo_url` omitting the secret, are ok. 200 that
/// stores and echoes `unique-secret` is a fail. Must not 500.
#[tokio::test]
async fn patch_fleet_config_repo_url_userinfo_not_persisted_or_echoed() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_other_email, other_jar, _other_ws) = signup_and_login(&app).await;
    let config_uri = fleet(&ws, "/config");
    let settings_uri = fleet(&ws, "/settings");
    let fleet_uri = fleet(&ws, "");
    let v1_cfg_uri = format!("/v1/projects/{ws}/config");
    let secret = "unique-secret";
    let oauth_basic = "x-oauth-basic";
    let userinfo_url = "https://user:unique-secret@example.test/acme.git";
    let oauth_url = "http://token:x-oauth-basic@github.com/acme/app.git";
    let clean_url = "https://github.com/acme/app.git";

    let primed = call(&app, "GET", &fleet_uri, Some(&jar), None).await;
    assert_eq!(primed.status, StatusCode::OK, "{}", primed.body);

    let patched = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": userinfo_url, "git_ref": "main" })),
    )
    .await;
    assert_ne!(
        patched.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "userinfo PATCH must not 500: {}",
        patched.raw
    );
    assert!(
        patched.status == StatusCode::BAD_REQUEST || patched.status == StatusCode::OK,
        "userinfo PATCH must 400 or 200-with-strip, not {}: {}",
        patched.status,
        patched.raw
    );
    if patched.status == StatusCode::BAD_REQUEST {
        assert!(
            !patched.raw.contains(secret),
            "400 raw body must not echo {secret}: {}",
            patched.raw
        );
    } else {
        assert!(
            !patched.raw.contains(secret),
            "200 PATCH raw body must not echo {secret}: {}",
            patched.raw
        );
        let stored = patched.body["config"]["repo_url"].as_str().unwrap_or("");
        assert!(
            !stored.contains(secret),
            "200 PATCH config.repo_url must omit {secret}: {}",
            patched.body
        );
        assert!(
            !https_url_has_userinfo(stored),
            "200 PATCH config.repo_url must omit userinfo: {}",
            patched.body
        );
    }

    assert_gets_omit_secrets(
        &app,
        &jar,
        &fleet_uri,
        &settings_uri,
        &v1_cfg_uri,
        &[secret],
    )
    .await;

    let oauth = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": oauth_url, "git_ref": "main" })),
    )
    .await;
    assert_ne!(
        oauth.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "oauth userinfo PATCH must not 500: {}",
        oauth.raw
    );
    assert!(
        oauth.status == StatusCode::BAD_REQUEST || oauth.status == StatusCode::OK,
        "oauth userinfo PATCH must 400 or 200-with-strip, not {}: {}",
        oauth.status,
        oauth.raw
    );
    if oauth.status == StatusCode::BAD_REQUEST {
        assert!(
            !oauth.raw.contains(secret) && !oauth.raw.contains(oauth_basic),
            "400 raw body must not echo {secret} or {oauth_basic}: {}",
            oauth.raw
        );
    } else {
        assert!(
            !oauth.raw.contains(secret) && !oauth.raw.contains(oauth_basic),
            "200 PATCH raw body must not echo {secret} or {oauth_basic}: {}",
            oauth.raw
        );
        let stored = oauth.body["config"]["repo_url"].as_str().unwrap_or("");
        assert!(
            !stored.contains(secret) && !stored.contains(oauth_basic),
            "200 PATCH config.repo_url must omit {secret} and {oauth_basic}: {}",
            oauth.body
        );
        assert!(
            !https_url_has_userinfo(stored),
            "200 PATCH config.repo_url must omit userinfo: {}",
            oauth.body
        );
    }

    assert_gets_omit_secrets(
        &app,
        &jar,
        &fleet_uri,
        &settings_uri,
        &v1_cfg_uri,
        &[secret, oauth_basic],
    )
    .await;

    let stolen = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&other_jar),
        Some(json!({ "repo_url": userinfo_url, "git_ref": "main" })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A's credential PATCH must 403: {}",
        stolen.raw
    );
    assert!(
        !stolen.raw.contains(secret) && !stolen.raw.contains(oauth_basic),
        "other-ws 403 raw body must not echo {secret} or {oauth_basic}: {}",
        stolen.raw
    );

    assert_gets_omit_secrets(
        &app,
        &jar,
        &fleet_uri,
        &settings_uri,
        &v1_cfg_uri,
        &[secret, oauth_basic],
    )
    .await;

    let control = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": clean_url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(control.status, StatusCode::OK, "{}", control.body);
    assert_eq!(control.body["config"]["repo_url"], clean_url);
    assert_eq!(control.body["config"]["git_ref"], "develop");

    let got = call(&app, "GET", &fleet_uri, Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["config"]["repo_url"], clean_url);
    assert_eq!(got.body["config"]["git_ref"], "develop");

    let settings = call(&app, "GET", &settings_uri, Some(&jar), None).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert_eq!(settings.body["repo_url"], clean_url);

    let cleared = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": "" })),
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert!(
        cleared.body["config"]["repo_url"].is_null(),
        "empty repo_url must still clear: {}",
        cleared.body
    );
    let after_clear = call(&app, "GET", &fleet_uri, Some(&jar), None).await;
    assert_eq!(after_clear.status, StatusCode::OK, "{}", after_clear.body);
    assert!(
        after_clear.body["config"]["repo_url"].is_null()
            || after_clear.body["config"]["repo_url"]
                .as_str()
                .map(str::trim)
                .unwrap_or("")
                .is_empty(),
        "GET /fleet after empty URL must show no remote: {}",
        after_clear.body
    );
}

fn https_url_has_userinfo(url: &str) -> bool {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .is_some_and(|rest| rest.contains('@'))
}

async fn assert_gets_omit_secrets(
    app: &axum::Router,
    jar: &CookieJar,
    fleet_uri: &str,
    settings_uri: &str,
    v1_cfg_uri: &str,
    secrets: &[&str],
) {
    let fleet_got = call(app, "GET", fleet_uri, Some(jar), None).await;
    assert_eq!(fleet_got.status, StatusCode::OK, "{}", fleet_got.body);
    let settings = call(app, "GET", settings_uri, Some(jar), None).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    let v1 = call_bearer(app, "GET", v1_cfg_uri, TEST_ADMIN_TOKEN, None).await;
    assert_eq!(v1.status, StatusCode::OK, "GET /v1 config: {}", v1.body);

    let fleet_url = fleet_got.body["config"]["repo_url"].as_str().unwrap_or("");
    let settings_url = settings.body["repo_url"].as_str().unwrap_or("");
    let v1_url = v1.body["repo_url"].as_str().unwrap_or("");

    for (label, out, url) in [
        ("GET /fleet", &fleet_got, fleet_url),
        ("GET /settings", &settings, settings_url),
        ("GET /v1/.../config", &v1, v1_url),
    ] {
        for secret in secrets {
            assert!(
                !out.raw.contains(secret),
                "{label} raw body must not contain {secret}: {}",
                out.raw
            );
            assert!(
                !url.contains(secret),
                "{label} repo_url must not contain {secret}: {url}"
            );
        }
        assert!(
            !https_url_has_userinfo(url),
            "{label} http(s) repo_url must omit userinfo: {url}"
        );
    }
}

/// Docs §6.9: PATCH `/fleet/config` empty URL clears the workspace git remote
/// (new tasks must not clone). Non-empty invalid URL is 400. `git_ref` without
/// a URL on a project that has no remote is 400. Failed PATCH must not mutate.
#[tokio::test]
async fn settings_empty_url_clears_git_remote_invalid_url_and_ref_without_url_are_400() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let project_id = Uuid::parse_str(&ws).expect("workspace id");
    let config_uri = fleet(&ws, "/config");
    let settings_uri = fleet(&ws, "/settings");
    let url = "https://github.com/acme/app.git";

    let primed = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(primed.status, StatusCode::OK, "{}", primed.body);

    let set = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "set remote: {}", set.body);
    assert_eq!(set.body["config"]["repo_url"], url);
    assert_eq!(set.body["config"]["git_ref"], "develop");

    let stored = state
        .orch
        .store
        .get_project_config(project_id)
        .await
        .expect("project config after set");
    let remote = stored
        .git_remote()
        .expect("set remote must persist as git_remote() for new spawns");
    assert_eq!(remote.url, url);
    assert_eq!(remote.git_ref, "develop");

    // Control: git_ref-only while a URL exists updates the ref, keeps the URL.
    // The 400 below is "ref without a remote", not "every git_ref PATCH is 400".
    let ref_ok = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(
        ref_ok.status,
        StatusCode::OK,
        "git_ref with existing URL: {}",
        ref_ok.body
    );
    assert_eq!(ref_ok.body["config"]["repo_url"], url);
    assert_eq!(ref_ok.body["config"]["git_ref"], "feature");

    for bad in [
        "not a url",
        "-e evil",
        "ftp://example.com/r.git",
        "https://example.com/has space.git",
        "javascript:alert(1)",
    ] {
        let out = call(
            &app,
            "PATCH",
            &config_uri,
            Some(&jar),
            Some(json!({ "repo_url": bad })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "invalid repo_url={bad:?} must 400: {}",
            out.body
        );
        let err = out.body["error"].as_str().unwrap_or("");
        assert!(
            err.contains("repo_url"),
            "invalid repo_url={bad:?} error should mention repo_url: {}",
            out.body
        );

        let got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
        assert_eq!(got.status, StatusCode::OK, "{}", got.body);
        assert_eq!(
            got.body["config"]["repo_url"], url,
            "invalid PATCH {bad:?} must not change repo_url: {}",
            got.body
        );
        assert_eq!(
            got.body["config"]["git_ref"], "feature",
            "invalid PATCH {bad:?} must not change git_ref: {}",
            got.body
        );
        let still = state
            .orch
            .store
            .get_project_config(project_id)
            .await
            .expect("project config after invalid PATCH");
        let still_remote = still
            .git_remote()
            .expect("invalid URL must leave the remote bound");
        assert_eq!(still_remote.url, url);
        assert_eq!(still_remote.git_ref, "feature");
    }

    let cleared = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": "" })),
    )
    .await;
    assert_eq!(
        cleared.status,
        StatusCode::OK,
        "empty URL: {}",
        cleared.body
    );
    assert!(
        cleared.body["config"]["repo_url"].is_null(),
        "empty repo_url must clear: {}",
        cleared.body
    );

    let after_clear = state
        .orch
        .store
        .get_project_config(project_id)
        .await
        .expect("project config after clear");
    assert!(
        after_clear.git_remote().is_none(),
        "empty URL must drop git_remote() so new tasks do not clone: {:?}",
        after_clear.repo_url
    );
    assert!(
        after_clear
            .repo_url
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty(),
        "cleared repo_url must be null/empty, got {:?}",
        after_clear.repo_url
    );

    for (label, uri) in [
        ("GET fleet", fleet(&ws, "")),
        ("GET settings", settings_uri.clone()),
    ] {
        let got = call(&app, "GET", &uri, Some(&jar), None).await;
        assert_eq!(got.status, StatusCode::OK, "{label}: {}", got.body);
        let repo = if label == "GET fleet" {
            &got.body["config"]["repo_url"]
        } else {
            &got.body["repo_url"]
        };
        assert!(
            repo.is_null() || repo.as_str().map(str::trim).unwrap_or("").is_empty(),
            "{label} after clear must show no remote: {}",
            got.body
        );
    }

    // New stub spawn after clear must not carry the previous clone URL.
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
    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    for session in sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let Some(work_dir) = session["work_dir"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        for name in ["task.json", "workspace.json", "prompt.md"] {
            let path = std::path::Path::new(work_dir).join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            assert!(
                !text.contains(url),
                "cleared remote must not leak into {name} (new tasks do not clone): {path:?}\n{text}"
            );
        }
        let workspace = std::path::Path::new(work_dir).join("workspace.json");
        if workspace.exists() {
            let text = std::fs::read_to_string(&workspace).unwrap_or_default();
            let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
            let leftover = parsed
                .pointer("/repo/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                leftover.is_empty(),
                "workspace.json after clear must not name a repo: {text}"
            );
        }
    }

    let ref_only = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "git_ref": "other" })),
    )
    .await;
    assert_eq!(
        ref_only.status,
        StatusCode::BAD_REQUEST,
        "git_ref without URL must 400: {}",
        ref_only.body
    );
    let err = ref_only.body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("repo_url"),
        "ref-without-URL error should mention repo_url: {}",
        ref_only.body
    );
    let still_clear = state
        .orch
        .store
        .get_project_config(project_id)
        .await
        .expect("project config after ref-only 400");
    assert!(
        still_clear.git_remote().is_none(),
        "ref-only 400 must not resurrect the remote: {:?}",
        still_clear.repo_url
    );

    // Empty URL with a git_ref is a clear (URL present but empty), not 400.
    let set_again = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(set_again.status, StatusCode::OK, "{}", set_again.body);
    let clear_with_ref = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": "", "git_ref": "other" })),
    )
    .await;
    assert_eq!(
        clear_with_ref.status,
        StatusCode::OK,
        "empty URL + git_ref must clear, not 400: {}",
        clear_with_ref.body
    );
    assert!(
        clear_with_ref.body["config"]["repo_url"].is_null(),
        "empty URL + git_ref must clear: {}",
        clear_with_ref.body
    );
    let after_empty_with_ref = state
        .orch
        .store
        .get_project_config(project_id)
        .await
        .expect("project config after empty+ref");
    assert!(
        after_empty_with_ref.git_remote().is_none(),
        "empty URL + git_ref must drop git_remote(): {:?}",
        after_empty_with_ref.repo_url
    );

    // Whitespace-only URL is empty after trim — also clears.
    let set_ws = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(set_ws.status, StatusCode::OK, "{}", set_ws.body);
    let ws_clear = call(
        &app,
        "PATCH",
        &config_uri,
        Some(&jar),
        Some(json!({ "repo_url": "   " })),
    )
    .await;
    assert_eq!(
        ws_clear.status,
        StatusCode::OK,
        "whitespace URL must clear: {}",
        ws_clear.body
    );
    assert!(
        ws_clear.body["config"]["repo_url"].is_null(),
        "whitespace repo_url must clear: {}",
        ws_clear.body
    );
    let after_ws = state
        .orch
        .store
        .get_project_config(project_id)
        .await
        .expect("project config after whitespace clear");
    assert!(
        after_ws.git_remote().is_none(),
        "whitespace URL must drop git_remote(): {:?}",
        after_ws.repo_url
    );
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

/// Distinctive prompt / live.jsonl / transcript / work_dir bytes from A's
/// GET /fleet/sessions/{id} 200. Empty strings are skipped so a later omit
/// check is not a tautology.
fn session_detail_file_needles(detail: &Value) -> Vec<String> {
    let mut needles = Vec::new();
    if let Some(dir) = detail
        .pointer("/session/work_dir")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        needles.push(dir.to_string());
    }
    for key in ["prompt", "transcript"] {
        let Some(text) = detail.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        needles.push(text.to_string());
        if let Ok(enc) = serde_json::to_string(text) {
            let inner = enc.trim_matches('"');
            if inner != text && !inner.is_empty() {
                needles.push(inner.to_string());
            }
        }
    }
    if let Some(live) = detail.get("live").and_then(|v| v.as_array()) {
        for ev in live {
            if let Some(text) = ev.get("text").and_then(|v| v.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    needles.push(text.to_string());
                }
            }
            let dumped = ev.to_string();
            if dumped.len() > 8 && dumped != "{}" && dumped != "null" {
                needles.push(dumped);
            }
        }
    }
    needles
}

fn assert_raw_omits_session_files(out: &HttpOut, label: &str, needles: &[String]) {
    for needle in needles {
        assert!(
            !out.raw.contains(needle),
            "{label} must not leak session file bytes ({:.80}): {}",
            needle,
            out.raw
        );
    }
}

fn assert_session_detail_omits_a_files(out: &HttpOut, label: &str, needles: &[String]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not be 200 with A's session: {}",
        out.body
    );
    if let Some(session) = out.body.get("session") {
        assert!(
            session.is_null() || session.as_object().is_some_and(|o| o.is_empty()),
            "{label} must not return A's session object: {}",
            out.body
        );
    }
    for key in ["prompt", "live", "transcript"] {
        match out.body.get(key) {
            None => {}
            Some(Value::Null) => {}
            Some(Value::String(s)) => {
                assert!(
                    s.trim().is_empty(),
                    "{label} must not include {key} bytes: {}",
                    out.body
                );
            }
            Some(Value::Array(items)) => {
                assert!(
                    items.is_empty(),
                    "{label} must not include {key} events: {}",
                    out.body
                );
            }
            Some(other) => panic!("{label} must not include {key}: {other}"),
        }
    }
    assert_raw_omits_session_files(out, label, needles);
}

/// Docs §6.4/§8.1: GET /fleet/sessions/{id} returns prompt / live.jsonl /
/// transcript from disk. Cookie BFF loads by session UUID then 404s if
/// `project_id != workspace_id`. Distinct from
/// [`workers_sandboxes_match_sessions_and_detail_shape`] (own session +
/// random UUID 404) and [`fleet_requires_membership`] (B on A's path 403
/// before lookup — not ownership of the id). Not session kill (TERM) or
/// run cancel. Filter `-- session_detail_foreign`.
/// catalog:list-session-detail-foreign-id-404
#[tokio::test]
async fn cookie_session_detail_foreign_id_is_404_and_leaks_no_files() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let _ = tick_until_tasks_done(&app, &a_jar, &a_ws, &["t1", "t2"]).await;

    let sessions = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let sid = sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find_map(|s| s["id"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("expected a session after stub run: {}", sessions.body));

    let snap = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /sessions/{{sid}} snapshot: {}",
        snap.body
    );
    assert!(
        snap.body["session"].is_object(),
        "snapshot session object: {}",
        snap.body
    );
    assert_eq!(
        snap.body["session"]["id"].as_str(),
        Some(sid.as_str()),
        "snapshot session.id: {}",
        snap.body
    );
    assert!(
        snap.body.get("prompt").is_some(),
        "snapshot prompt key: {}",
        snap.body
    );
    assert!(
        snap.body.get("live").is_some(),
        "snapshot live key: {}",
        snap.body
    );
    assert!(
        snap.body.get("transcript").is_some(),
        "snapshot transcript key: {}",
        snap.body
    );
    let work_dir = snap.body["session"]["work_dir"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        !work_dir.is_empty(),
        "snapshot work_dir must be non-empty so the leak check is not vacuous: {}",
        snap.body
    );
    let prompt = snap.body["prompt"].as_str().unwrap_or("").to_string();
    assert!(
        !prompt.trim().is_empty(),
        "snapshot prompt must be non-empty so the leak check is not vacuous: {}",
        snap.body
    );
    let needles = session_detail_file_needles(&snap.body);
    assert!(
        needles.iter().any(|n| n == &work_dir),
        "needles must include work_dir: {needles:?}"
    );
    assert!(
        needles
            .iter()
            .any(|n| n.contains(prompt.trim()) || n == &prompt),
        "needles must include prompt text so 404 omit is not vacuous: {needles:?}"
    );

    let b_fleet = call(&app, "GET", &fleet(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_fleet.status,
        StatusCode::OK,
        "B GET /fleet must prime B's project so this is not a missing-project tautology: {}",
        b_fleet.body
    );

    let stolen_on_b = call(
        &app,
        "GET",
        &fleet(&b_ws, &format!("/sessions/{sid}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        stolen_on_b.status,
        StatusCode::NOT_FOUND,
        "B's cookie on B's path with A's session_id must 404, not 200 with A's session: {}",
        stolen_on_b.body
    );
    assert_ne!(
        stolen_on_b.body.get("session").and_then(|s| s.get("id")),
        Some(&json!(sid)),
        "404 must not return A's session id as the row: {}",
        stolen_on_b.body
    );
    assert_session_detail_omits_a_files(
        &stolen_on_b,
        "B GET /sessions/{A_sid} on B's path",
        &needles,
    );

    let b_list = call(&app, "GET", &fleet(&b_ws, "/sessions"), Some(&b_jar), None).await;
    assert_eq!(
        b_list.status,
        StatusCode::OK,
        "B GET /sessions: {}",
        b_list.body
    );
    let b_ids: Vec<String> = b_list.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        !b_ids.iter().any(|id| id == &sid),
        "B's GET /sessions list must not include A's session_id {sid}: {}",
        b_list.body
    );
    assert_raw_omits_session_files(&b_list, "B GET /sessions list", &needles);

    let stolen_on_a = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        stolen_on_a.status,
        StatusCode::FORBIDDEN,
        "B's cookie on A's path must 403 not-a-member (same as fleet_requires_membership): {}",
        stolen_on_a.body
    );
    let stolen_a_err = http_error_text(&stolen_on_a).to_ascii_lowercase();
    assert!(
        stolen_a_err.contains("not a member"),
        "403 must say not a member of this workspace: {}",
        stolen_on_a.body
    );
    assert_session_detail_omits_a_files(&stolen_on_a, "B GET A's /sessions/{A_sid}", &needles);

    let again = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::OK,
        "control: A's cookie on A's path must still 200 so this is not a tautology that every detail is 404: {}",
        again.body
    );
    assert_eq!(
        again.body["session"]["id"].as_str(),
        Some(sid.as_str()),
        "control session.id: {}",
        again.body
    );
    assert!(
        again.body.get("prompt").is_some(),
        "control prompt: {}",
        again.body
    );
    assert!(
        again.body.get("live").is_some(),
        "control live: {}",
        again.body
    );
    assert!(
        again.body.get("transcript").is_some(),
        "control transcript: {}",
        again.body
    );
    assert_eq!(
        again.body["session"]["work_dir"].as_str(),
        Some(work_dir.as_str()),
        "control work_dir: {}",
        again.body
    );
    assert_eq!(
        again.body["prompt"].as_str(),
        Some(prompt.as_str()),
        "control prompt text: {}",
        again.body
    );
}

/// Cookie BFF POST /fleet/sessions/{id}/kill is membership- and project-scoped.
/// Knowing A's session UUID is not enough: B's cookie on B's path must not
/// TERM A's sandbox (`kill_session` looks up id then 404s if
/// `project_id != workspace_id`); B on A's path is 403 not-a-member.
/// Distinct from [`cookie_session_detail_foreign_id_is_404_and_leaks_no_files`]
/// (GET detail), [`fleet_requires_membership`] (B on A's GET /fleet), and
/// [`cookie_cancel_foreign_run_id_does_not_mark_cancelled`] (run cancel).
/// Filter `-- kill`.
/// catalog:list-session-kill-cross-workspace-403
#[tokio::test]
async fn cookie_session_kill_foreign_id_is_404_or_403_and_does_not_term() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    let _ = tick_until_tasks_done(&app, &a_jar, &a_ws, &["t1", "t2"]).await;

    let listed = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let sid = listed.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find_map(|s| s["id"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("expected a session after stub run: {}", listed.body));

    let snap = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /sessions/{{sid}} snapshot: {}",
        snap.body
    );
    assert!(
        snap.body["session"].is_object(),
        "snapshot session object: {}",
        snap.body
    );
    assert_eq!(
        snap.body["session"]["id"].as_str(),
        Some(sid.as_str()),
        "snapshot session.id: {}",
        snap.body
    );
    let snap_status = snap.body["session"]["status"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        !snap_status.is_empty(),
        "snapshot session.status must be present: {}",
        snap.body
    );
    assert_session_not_killing(&snap, &listed, &sid, "A snapshot after fixture");

    let b_fleet = call(&app, "GET", &fleet(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_fleet.status,
        StatusCode::OK,
        "B GET /fleet must prime B's project so this is not a missing-project tautology: {}",
        b_fleet.body
    );

    let stolen_on_b = call(
        &app,
        "POST",
        &fleet(&b_ws, &format!("/sessions/{sid}/kill")),
        Some(&b_jar),
        Some(json!({ "reason": "b-foreign-kill" })),
    )
    .await;
    assert_foreign_session_kill_rejected(
        &stolen_on_b,
        "B's cookie on B's path with A's session_id",
    );

    let after_b_path_detail = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    let after_b_path_list = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_session_not_killing(
        &after_b_path_detail,
        &after_b_path_list,
        &sid,
        "A GET after B kill on B's path",
    );

    let stolen_on_a = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/sessions/{sid}/kill")),
        Some(&b_jar),
        Some(json!({ "reason": "b-on-a-kill" })),
    )
    .await;
    assert_eq!(
        stolen_on_a.status,
        StatusCode::FORBIDDEN,
        "B's cookie on A's path must 403 not-a-member: {}",
        stolen_on_a.body
    );
    let stolen_a_err = http_error_text(&stolen_on_a).to_ascii_lowercase();
    assert!(
        stolen_a_err.contains("not a member"),
        "403 must say not a member of this workspace: {}",
        stolen_on_a.body
    );
    assert_ne!(
        stolen_on_a.body.get("killed"),
        Some(&json!(true)),
        "403 must not claim killed: {}",
        stolen_on_a.body
    );

    let after_403_detail = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    let after_403_list = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_session_not_killing(
        &after_403_detail,
        &after_403_list,
        &sid,
        "A GET after B 403 on A's path",
    );

    let unauth = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/sessions/{sid}/kill")),
        None,
        Some(json!({ "reason": "unauth-kill" })),
    )
    .await;
    assert_eq!(
        unauth.status,
        StatusCode::UNAUTHORIZED,
        "unauth POST kill must 401: {}",
        unauth.body
    );
    assert_ne!(
        unauth.body.get("killed"),
        Some(&json!(true)),
        "401 must not claim killed: {}",
        unauth.body
    );
    let after_unauth_detail = call(
        &app,
        "GET",
        &fleet(&a_ws, &format!("/sessions/{sid}")),
        Some(&a_jar),
        None,
    )
    .await;
    let after_unauth_list = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_session_not_killing(
        &after_unauth_detail,
        &after_unauth_list,
        &sid,
        "A GET after unauth kill",
    );

    let missing = Uuid::new_v4();
    let unknown = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/sessions/{missing}/kill")),
        Some(&a_jar),
        Some(json!({ "reason": "unknown-kill" })),
    )
    .await;
    assert_eq!(
        unknown.status,
        StatusCode::NOT_FOUND,
        "unknown session UUID POST kill must 404: {}",
        unknown.body
    );
    assert_ne!(
        unknown.body.get("killed"),
        Some(&json!(true)),
        "unknown kill 404 must not claim killed: {}",
        unknown.body
    );

    let own = call(
        &app,
        "POST",
        &fleet(&a_ws, &format!("/sessions/{sid}/kill")),
        Some(&a_jar),
        Some(json!({ "reason": "a-own-kill" })),
    )
    .await;
    assert_eq!(
        own.status,
        StatusCode::OK,
        "A's cookie on A's live/exited session kill must 200 killed or already_dead, not {}: {}",
        own.status,
        own.body
    );
    let killed = own.body.get("killed") == Some(&json!(true));
    let already_dead = own.body.get("already_dead") == Some(&json!(true));
    assert!(
        killed || already_dead,
        "owner 200 must report killed or already_dead (404 is only the unknown-UUID probe): {}",
        own.body
    );
    assert_eq!(
        own.body["session_id"].as_str(),
        Some(sid.as_str()),
        "owner kill session_id: {}",
        own.body
    );
}

fn assert_foreign_session_kill_rejected(out: &HttpOut, label: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not 200 killing A's session: {}",
        out.body
    );
    assert!(
        out.status == StatusCode::NOT_FOUND || out.status == StatusCode::FORBIDDEN,
        "{label} must be 404 or 403, not {}: {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.body.get("killed"),
        Some(&json!(true)),
        "{label} must not claim killed: {}",
        out.body
    );
}

fn assert_session_not_killing(detail: &HttpOut, list: &HttpOut, sid: &str, label: &str) {
    assert_eq!(
        detail.status,
        StatusCode::OK,
        "{label} detail must stay 200: {}",
        detail.body
    );
    assert_eq!(
        detail.body["session"]["id"].as_str(),
        Some(sid),
        "{label} detail session.id: {}",
        detail.body
    );
    let detail_status = detail.body["session"]["status"].as_str().unwrap_or("");
    assert!(
        detail_status != "killing" && detail_status != "killed",
        "{label} detail status must not be killing/killed: {}",
        detail.body
    );
    assert_eq!(list.status, StatusCode::OK, "{label} list: {}", list.body);
    let row = list.body["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"].as_str() == Some(sid))
        .unwrap_or_else(|| panic!("{label}: sid {sid} missing from list: {}", list.body));
    let list_status = row["status"].as_str().unwrap_or("");
    assert!(
        list_status != "killing" && list_status != "killed",
        "{label} list status must not be killing/killed: {row}"
    );
}

/// Cookie BFF + admin GET /v1/workers must not echo worker_token or its hash
/// (Docs §8.2: Bearer <session worker_token> is heartbeat auth).
#[tokio::test]
async fn sessions_omit_worker_token_and_hash_from_cookie_and_admin_gets() {
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
    let _ = tick_until_tasks_done(&app, &jar, &ws, &["t1", "t2"]).await;

    let sessions = call(&app, "GET", &fleet(&ws, "/sessions"), Some(&jar), None).await;
    assert_eq!(sessions.status, StatusCode::OK, "{}", sessions.body);
    let sid = sessions.body["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find_map(|s| s["id"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("expected a session after stub run: {}", sessions.body));
    let sid_uuid = Uuid::parse_str(&sid).expect("session id");
    let hash: String =
        sqlx::query_scalar("SELECT worker_token_hash FROM worker_sessions WHERE id = $1")
            .bind(sid_uuid)
            .fetch_one(state.orch.store.pool())
            .await
            .expect("worker_token_hash");
    assert!(
        !hash.is_empty(),
        "worker_sessions.worker_token_hash must be stored so the omit check is not a tautology"
    );

    let detail = call(
        &app,
        "GET",
        &fleet(&ws, &format!("/sessions/{sid}")),
        Some(&jar),
        None,
    )
    .await;
    let workers = call(&app, "GET", &fleet(&ws, "/workers"), Some(&jar), None).await;
    let overview = call(&app, "GET", &fleet(&ws, "/overview"), Some(&jar), None).await;
    for (label, out, objects) in [
        ("GET /sessions", &sessions, sessions.body.get("sessions")),
        ("GET /sessions/{sid}", &detail, detail.body.get("session")),
        ("GET /workers", &workers, workers.body.get("sandboxes")),
        ("GET /overview", &overview, None),
    ] {
        assert_eq!(out.status, StatusCode::OK, "{label}: {}", out.body);
        assert_session_objects_omit_worker_token(objects, label, &out.body);
        assert_raw_omits_worker_token_secret(out, &hash, label);
    }

    let admin = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{ws}/workers"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        admin.status,
        StatusCode::OK,
        "admin GET /v1/workers: {}",
        admin.body
    );
    assert_session_objects_omit_worker_token(
        admin.body.get("workers"),
        "admin GET /v1/workers",
        &admin.body,
    );
    assert_raw_omits_worker_token_secret(&admin, &hash, "admin GET /v1/workers");

    let cookie_v1 = call(
        &app,
        "GET",
        &format!("/v1/projects/{ws}/workers"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        cookie_v1.status,
        StatusCode::UNAUTHORIZED,
        "session cookie must not authorize GET /v1/projects/{{id}}/workers: {}",
        cookie_v1.body
    );

    assert_eq!(
        detail.body["session"]["id"].as_str(),
        Some(sid.as_str()),
        "control: session.id must match: {}",
        detail.body
    );
    assert!(
        detail.body.get("prompt").is_some(),
        "control prompt: {}",
        detail.body
    );
    assert!(
        detail.body.get("live").is_some(),
        "control live: {}",
        detail.body
    );
    assert!(
        detail.body.get("transcript").is_some(),
        "control transcript: {}",
        detail.body
    );

    let garbage = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{ws}/workers/{sid}/heartbeat"),
        "garbage-not-a-worker-token",
        Some(json!({ "seq": 1 })),
    )
    .await;
    assert_eq!(
        garbage.status,
        StatusCode::UNAUTHORIZED,
        "garbage Bearer on /v1 heartbeat must stay 401: {}",
        garbage.body
    );
}

fn assert_session_objects_omit_worker_token(node: Option<&Value>, label: &str, body: &Value) {
    const FORBIDDEN: &[&str] = &["worker_token", "worker_token_hash", "FLEET_WORKER_TOKEN"];
    match node {
        None => {}
        Some(Value::Array(items)) => {
            assert!(
                !items.is_empty(),
                "{label} session/sandbox list must not be empty: {body}"
            );
            for (i, item) in items.iter().enumerate() {
                let Some(obj) = item.as_object() else {
                    panic!("{label}[{i}] must be an object: {body}");
                };
                for key in FORBIDDEN {
                    assert!(
                        !obj.contains_key(*key),
                        "{label}[{i}] must omit {key}: {body}"
                    );
                }
            }
        }
        Some(Value::Object(obj)) => {
            for key in FORBIDDEN {
                assert!(!obj.contains_key(*key), "{label} must omit {key}: {body}");
            }
        }
        Some(other) => panic!("{label} session/sandbox node must be object or array: {other}"),
    }
}

fn assert_raw_omits_worker_token_secret(out: &HttpOut, hash: &str, label: &str) {
    assert!(
        !out.raw.contains(hash),
        "{label} raw body must not contain worker_token_hash hex: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("FLEET_WORKER_TOKEN"),
        "{label} raw body must not contain FLEET_WORKER_TOKEN: {}",
        out.raw
    );
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
    assert_eq!(archived.status, StatusCode::NO_CONTENT, "{}", archived.body);

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

    // No fleet_access cookie: rejected *before* inner (401 popup HTML).
    // Trello-without-code is the only public 200 (fragment capture).
    let cb = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/github",
        None,
        None,
    )
    .await;
    assert_eq!(
        cb.status,
        StatusCode::UNAUTHORIZED,
        "oauth callback without cookie must 401 not authenticated: {} {}",
        cb.status,
        cb.raw
    );
    assert_oauth_callback_html(&cb, "github callback without cookie");
    let cb_l = cb.raw.to_ascii_lowercase();
    assert!(
        cb_l.contains("not authenticated"),
        "no-cookie callback must say not authenticated: {}",
        cb.raw
    );
    assert!(
        callback_reports_ok_false(&cb.raw),
        "no-cookie popup must report ok=false: {}",
        cb.raw
    );
    assert!(
        !cb.raw.contains("access_token"),
        "callback must not leak access_token: {}",
        cb.raw
    );

    // No-code close page: member cookie so oauth_callback_inner runs
    // (missing code/state), not the no-session 401.
    let cb_inner = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/github",
        Some(&jar),
        None,
    )
    .await;
    assert!(
        !cb_inner.status.is_server_error(),
        "member no-code oauth callback must not 5xx: {} {}",
        cb_inner.status,
        cb_inner.raw
    );
    assert_ne!(
        cb_inner.status,
        StatusCode::NOT_FOUND,
        "member no-code callback must not 404: {}",
        cb_inner.raw
    );
    assert_oauth_callback_html(&cb_inner, "member github callback without code");
    let inner_l = cb_inner.raw.to_ascii_lowercase();
    assert!(
        !inner_l.contains("not authenticated"),
        "member no-code callback must reach inner, not the no-cookie 401: {}",
        cb_inner.raw
    );
    assert!(
        !cb_inner.raw.contains("access_token"),
        "member no-code callback must not leak access_token: {}",
        cb_inner.raw
    );

    // typ=access state JWT with a member cookie so verify_oauth_state runs.
    // Mirrors fleet_integrations oauth state_token unit; exercises the HTTP path.
    let bad_state = oauth_state_with_typ_access(TEST_JWT_SECRET, "github");
    let cb_access = call(
        &app,
        "GET",
        &format!("/api/integrations/oauth/callback/github?code=fake-code&state={bad_state}"),
        Some(&jar),
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
        StatusCode::UNAUTHORIZED,
        "typ=access must 401 HTML naming invalid/expired state, not the no-cookie page: {}",
        cb_access.raw
    );
    assert_oauth_callback_html(&cb_access, "typ=access github callback");
    let lower = cb_access.raw.to_ascii_lowercase();
    assert!(
        (lower.contains("invalid") || lower.contains("expired")) && lower.contains("state"),
        "typ=access must name invalid/expired state, not merely Connection failed: {}",
        cb_access.raw
    );
    assert!(
        !lower.contains("not authenticated"),
        "typ=access with cookie must not be the no-session 401: {}",
        cb_access.raw
    );
    assert!(
        callback_reports_ok_false(&cb_access.raw),
        "popup payload must report ok=false: {}",
        cb_access.raw
    );
    assert!(
        !cb_access.raw.contains("access_token"),
        "must not leak access_token: {}",
        cb_access.raw
    );
}

const OAUTH_START_EVIL_HOST: &str = "evil.example.test";

fn oauth_start_spoofed_host_headers() -> [(&'static str, &'static str); 3] {
    [
        ("Host", OAUTH_START_EVIL_HOST),
        ("X-Forwarded-Host", OAUTH_START_EVIL_HOST),
        ("X-Forwarded-Proto", "https"),
    ]
}

fn oauth_start_api_public_callback(provider: &str) -> String {
    format!("http://localhost:8080/api/integrations/oauth/callback/{provider}")
}

fn oauth_start_api_public_callback_enc(provider: &str) -> String {
    format!("http%3A%2F%2Flocalhost%3A8080%2Fapi%2Fintegrations%2Foauth%2Fcallback%2F{provider}")
}

fn haystack_has_api_public_callback(hay: &str, provider: &str) -> bool {
    let raw = oauth_start_api_public_callback(provider);
    let enc = oauth_start_api_public_callback_enc(provider);
    hay.contains(&raw) || hay.to_ascii_lowercase().contains(&enc.to_ascii_lowercase())
}

/// Host / X-Forwarded-Host must not become redirect_uri / callback_urls / redirect_url.
fn assert_oauth_start_omits_evil_host(hay: &str, label: &str) {
    let lower = hay.to_ascii_lowercase();
    for needle in [
        OAUTH_START_EVIL_HOST,
        "https://evil.example.test",
        "http://evil.example.test",
        "https%3a%2f%2fevil.example.test",
        "http%3a%2f%2fevil.example.test",
        "evil%2eexample%2etest",
    ] {
        assert!(
            !lower.contains(needle),
            "{label} must not mint a callback on {needle}: {hay}"
        );
    }
    for key in ["redirect_uri", "callback_urls", "redirect_url"] {
        if let Some(idx) = lower.find(key) {
            let window = &lower[idx..lower.len().min(idx + key.len() + 160)];
            assert!(
                !window.contains(OAUTH_START_EVIL_HOST)
                    && !window.contains("https%3a%2f%2fevil.example.test")
                    && !window.contains("http%3a%2f%2fevil.example.test"),
                "{label} {key} origin must not be {OAUTH_START_EVIL_HOST}: {window}"
            );
        }
    }
}

fn assert_oauth_start_bootstrap_html(out: &HttpOut, provider: &str, label: &str) {
    assert!(
        !out.status.is_server_error(),
        "{label} must not 5xx: {} {}",
        out.status,
        out.raw
    );
    assert_eq!(
        out.status,
        StatusCode::OK,
        "{label} unconfigured start is 200 HTML, not 302/4xx/5xx: {} {}",
        out.status,
        out.raw
    );
    let ct = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/html"),
        "{label} expected HTML, content-type={ct:?} body={}",
        out.raw
    );
    let hay = format!("{} {} {}", out.raw, out.body, response_location(out));
    assert!(
        haystack_has_api_public_callback(&hay, provider),
        "{label} body must include API_PUBLIC_BASE callback {}: {}",
        oauth_start_api_public_callback(provider),
        out.raw
    );
    assert_oauth_start_omits_evil_host(&hay, label);
    if provider == "github" {
        assert!(
            out.raw.contains("github.com/settings/apps/new")
                && out.raw.contains("name=\"manifest\""),
            "{label} GitHub start should POST an App Manifest: {}",
            out.raw
        );
        assert!(
            out.raw
                .contains("http://localhost:8080/api/integrations/oauth/github/manifest/callback")
                || out.raw.to_ascii_lowercase().contains(
                    "http%3a%2f%2flocalhost%3a8080%2fapi%2fintegrations%2foauth%2fgithub%2fmanifest%2fcallback"
                ),
            "{label} GitHub manifest redirect_url must be API_PUBLIC_BASE manifest/callback: {}",
            out.raw
        );
    } else {
        assert!(
            out.raw.contains("client_id") && out.raw.contains("client_secret"),
            "{label} {provider} start should ask for an OAuth app: {}",
            out.raw
        );
        assert!(
            out.raw.contains("Authorization callback URL"),
            "{label} {provider} bootstrap must show the callback URL: {}",
            out.raw
        );
    }
}

fn assert_oauth_start_redirects_to_api_public_base(out: &HttpOut, provider: &str, label: &str) {
    assert!(
        !out.status.is_server_error(),
        "{label} must not 5xx: {} {}",
        out.status,
        out.raw
    );
    assert!(
        out.status == StatusCode::FOUND || out.status == StatusCode::TEMPORARY_REDIRECT,
        "{label} configured start must 302/307 to the provider, got {} {}",
        out.status,
        out.raw
    );
    let loc = response_location(out);
    assert!(
        !loc.is_empty(),
        "{label} must set Location: {} {}",
        out.status,
        out.raw
    );
    let hay = format!("{} {} {}", out.raw, out.body, loc);
    assert!(
        haystack_has_api_public_callback(&hay, provider),
        "{label} Location redirect_uri must be API_PUBLIC_BASE {}: {loc}",
        oauth_start_api_public_callback(provider)
    );
    assert!(
        loc.contains("redirect_uri="),
        "{label} Location must carry redirect_uri: {loc}"
    );
    assert_oauth_start_omits_evil_host(&hay, label);
}

/// Host / X-Forwarded-Host on oauth/start must not become the provider
/// redirect_uri / GitHub App Manifest callback origin. Distinct from
/// [`integrations_catalog_oauth_start_delete_and_callback_contracts`] (no Host)
/// and from list-oauth-start-unconfigured-still-bootstraps (helper, no HTTP Host).
#[tokio::test]
async fn oauth_start_ignores_host_header_for_callback_urls() {
    let (app, _state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(ws, b_ws, "A and B must own distinct workspaces");

    let spoofed = oauth_start_spoofed_host_headers();

    for provider in ["github", "gitlab"] {
        let path = format!("/api/workspaces/{ws}/integrations/{provider}/oauth/start");

        let control = call(&app, "GET", &path, Some(&jar), None).await;
        assert_oauth_start_bootstrap_html(
            &control,
            provider,
            &format!("no-Host GET {provider} oauth/start"),
        );

        let spoofed_out = call_inner(&app, "GET", &path, Some(&jar), None, &spoofed, None).await;
        assert_oauth_start_bootstrap_html(
            &spoofed_out,
            provider,
            &format!("Host+X-Forwarded-Host GET {provider} oauth/start"),
        );

        let as_b = call_inner(&app, "GET", &path, Some(&b_jar), None, &spoofed, None).await;
        assert_integrations_other_ws_forbidden(
            &as_b,
            &ws,
            &path,
            &format!("B GET A's {provider} oauth/start with spoofed Host"),
        );
        assert!(
            !as_b.status.is_server_error(),
            "other-ws oauth/start must not 5xx: {} {}",
            as_b.status,
            as_b.raw
        );
        let b_hay = format!("{} {} {}", as_b.raw, as_b.body, response_location(&as_b));
        assert_oauth_start_omits_evil_host(
            &b_hay,
            &format!("B GET A's {provider} oauth/start with spoofed Host"),
        );
    }

    let health = call_inner(&app, "GET", "/health", None, None, &spoofed, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health with Host: {OAUTH_START_EVIL_HOST} must stay 200 so this is not a tautology that Host 400s: {}",
        health.body
    );

    let mut configured = test_integrations_config(TEST_JWT_SECRET);
    configured.github = Some(OAuthClientConfig {
        client_id: "Iv1.host-header-lock-client".into(),
        client_secret: "host-header-lock-secret".into(),
        github_app_slug: None,
    });
    configured.gitlab = Some(OAuthClientConfig {
        client_id: "gl-host-header-lock-client".into(),
        client_secret: "gl-host-header-lock-secret".into(),
        github_app_slug: None,
    });
    let (app_cfg, _) = app_with_integrations(configured).await;
    let (_cfg_email, cfg_jar, cfg_ws) = signup_and_login(&app_cfg).await;
    let (_cfg_b_email, cfg_b_jar, cfg_b_ws) = signup_and_login(&app_cfg).await;
    assert_ne!(cfg_ws, cfg_b_ws, "configured fixture A/B must differ");

    for provider in ["github", "gitlab"] {
        let path = format!("/api/workspaces/{cfg_ws}/integrations/{provider}/oauth/start");

        let no_host = call(&app_cfg, "GET", &path, Some(&cfg_jar), None).await;
        assert_oauth_start_redirects_to_api_public_base(
            &no_host,
            provider,
            &format!("configured no-Host GET {provider} oauth/start"),
        );

        let with_host =
            call_inner(&app_cfg, "GET", &path, Some(&cfg_jar), None, &spoofed, None).await;
        assert_oauth_start_redirects_to_api_public_base(
            &with_host,
            provider,
            &format!("configured Host+X-Forwarded-Host GET {provider} oauth/start"),
        );

        let as_b = call_inner(
            &app_cfg,
            "GET",
            &path,
            Some(&cfg_b_jar),
            None,
            &spoofed,
            None,
        )
        .await;
        assert_integrations_other_ws_forbidden(
            &as_b,
            &cfg_ws,
            &path,
            &format!("B GET A's configured {provider} oauth/start with spoofed Host"),
        );
        assert_ne!(
            as_b.status,
            StatusCode::FOUND,
            "B configured oauth/start must not 302: {} loc={}",
            as_b.status,
            response_location(&as_b)
        );
        assert_ne!(
            as_b.status,
            StatusCode::TEMPORARY_REDIRECT,
            "B configured oauth/start must not 307: {} loc={}",
            as_b.status,
            response_location(&as_b)
        );
    }
}

/// Provider `error` / `error_description` on the public OAuth callback must
/// land in the popup-close HTML only as escaped HTML/JS. Must not 5xx, leak
/// tokens, or upsert a connection. Distinct from the no-code catalog
/// contract (no error query), UI `/oauth/done`, callback bind / session /
/// expiry / provider, and GitHub manifest persist / webhook unsigned.
#[tokio::test]
async fn oauth_callback_error_html_is_escaped_and_does_not_persist() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");

    assert_oauth_error_callback_not_persisted(
        &app,
        &state,
        &jar,
        &ws,
        workspace_id,
        user_id,
        "fresh workspace before error callbacks",
    )
    .await;

    let probe_q = oauth_provider_error_query(OAUTH_CALLBACK_ERROR_PROBE);

    // 1. Unauthenticated GET callback/{github,gitlab} with a denial string
    // that would break out of the popup HTML/JS if interpolated raw.
    for provider in ["github", "gitlab"] {
        let out = call(
            &app,
            "GET",
            &format!("/api/integrations/oauth/callback/{provider}?{probe_q}"),
            None,
            None,
        )
        .await;
        assert_oauth_callback_error_html_closed(
            &out,
            &format!("anon GET oauth/callback/{provider} error_description probe"),
            /*expect_interpolated=*/ false,
        );
    }

    // Authenticated GET: oauth_callback_inner returns early on q.error and
    // oauth_popup_close_html interpolates error_description. This is the
    // HTTP lock on html_escape / json_escape (not a helper unit).
    for provider in ["github", "gitlab"] {
        let out = call(
            &app,
            "GET",
            &format!("/api/integrations/oauth/callback/{provider}?{probe_q}"),
            Some(&jar),
            None,
        )
        .await;
        assert_oauth_callback_error_html_closed(
            &out,
            &format!("owner GET oauth/callback/{provider} error_description probe"),
            /*expect_interpolated=*/ true,
        );
    }

    // 3. Same query on the public GitHub App Manifest callback.
    let manifest = call(
        &app,
        "GET",
        &format!("/api/integrations/oauth/github/manifest/callback?{probe_q}"),
        None,
        None,
    )
    .await;
    assert_oauth_callback_error_html_closed(
        &manifest,
        "anon GET github/manifest/callback error_description probe",
        /*expect_interpolated=*/ true,
    );
    let loc = response_location(&manifest);
    assert_ne!(
        manifest.status,
        StatusCode::FOUND,
        "manifest error must not 302 to persist/install: {} loc={loc}",
        manifest.status
    );
    assert_ne!(
        manifest.status,
        StatusCode::TEMPORARY_REDIRECT,
        "manifest error must not 307 to persist/install: {} loc={loc}",
        manifest.status
    );

    // 2. Owner catalog + credentials/connections stay disconnected.
    assert_oauth_error_callback_not_persisted(
        &app,
        &state,
        &jar,
        &ws,
        workspace_id,
        user_id,
        "after error_description probes",
    )
    .await;

    // 4. Control: /health still 200; a plain provider denial is 200 HTML
    // "Connection failed" so this is not a tautology that every callback 404s.
    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200: {}",
        health.body
    );

    let denied_q = oauth_provider_error_query("denied");
    let denied = call(
        &app,
        "GET",
        &format!("/api/integrations/oauth/callback/github?{denied_q}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        denied.status,
        StatusCode::OK,
        "plain access_denied callback must be 200 HTML, not 404: {} {}",
        denied.status,
        denied.raw
    );
    let denied_ct = denied
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        denied_ct.contains("text/html"),
        "plain denial must be HTML, content-type={denied_ct:?} body={}",
        denied.raw
    );
    let denied_l = denied.raw.to_ascii_lowercase();
    assert!(
        denied_l.contains("connection failed")
            || denied_l.contains("could not connect")
            || denied_l.contains("denied"),
        "plain denial must show Connection failed (or equivalent): {}",
        denied.raw
    );
    assert!(
        denied.raw.contains("<html") || denied_l.contains("<html"),
        "plain denial must be HTML: {}",
        denied.raw
    );

    // Missing-code (no error) with a member cookie still hits inner's
    // close page, not 404 / not the no-cookie 401.
    let no_code = call(
        &app,
        "GET",
        "/api/integrations/oauth/callback/github",
        Some(&jar),
        None,
    )
    .await;
    assert!(
        !no_code.status.is_server_error(),
        "missing-code callback must not 5xx: {} {}",
        no_code.status,
        no_code.raw
    );
    assert_ne!(
        no_code.status,
        StatusCode::NOT_FOUND,
        "missing-code callback must not 404: {}",
        no_code.raw
    );
    assert_oauth_callback_html(&no_code, "member missing-code github callback");
    assert!(
        !no_code.raw.to_ascii_lowercase().contains("not authenticated"),
        "member missing-code callback must reach inner, not the no-cookie 401: {}",
        no_code.raw
    );
    assert!(
        !no_code.raw.contains("access_token"),
        "missing-code callback must not leak access_token: {}",
        no_code.raw
    );
}

/// Quote + angle brackets + marker + a closing-tag fragment. Not a working
/// exploit — the raw form must never appear in popup HTML or the inline script.
const OAUTH_CALLBACK_ERROR_PROBE: &str = r#""></div><script>raw-callback-probe</script>"#;

fn oauth_provider_error_query(error_description: &str) -> String {
    format!(
        "error=access_denied&error_description={}",
        percent_encode_query(error_description)
    )
}

fn percent_encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Popup-close HTML: real `Content-Type: text/html` and an `<html` tag in
/// `raw`. `call_inner` wraps every non-JSON body as `{ "raw": raw }`, so
/// `body.get("raw")` is not an HTML signal.
fn assert_oauth_callback_html(out: &HttpOut, label: &str) {
    let ct = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.to_ascii_lowercase().contains("text/html"),
        "{label} must be Content-Type text/html, got {ct:?}: {}",
        out.raw
    );
    assert!(
        out.raw.to_ascii_lowercase().contains("<html"),
        "{label} must contain <html: {}",
        out.raw
    );
}

/// Public / session callback with `error` must not 5xx, leak secrets, or
/// interpolate the probe as raw HTML/JS. 200 HTML is ok.
fn assert_oauth_callback_error_html_closed(out: &HttpOut, label: &str, expect_interpolated: bool) {
    assert!(
        !out.status.is_server_error(),
        "{label} must not 5xx: {} {}",
        out.status,
        out.raw
    );
    assert_ne!(
        out.status,
        StatusCode::NOT_FOUND,
        "{label} must not 404: {}",
        out.raw
    );
    let clientish = matches!(
        out.status,
        StatusCode::OK | StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
    );
    assert!(
        clientish,
        "{label} must be 200/400/401, got {} {}",
        out.status,
        out.raw
    );
    assert_oauth_callback_html(out, label);

    let hay = format!("{} {}", out.raw, out.body);
    for needle in [
        "access_token",
        "refresh_token",
        "client_secret",
        "webhook_secret",
    ] {
        assert!(
            !hay.contains(needle),
            "{label} must not leak {needle}: {}",
            out.raw
        );
    }
    // "pem" as a whole token, not a substring of "temporary".
    assert!(
        !hay.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|tok| tok == "pem"),
        "{label} must not leak pem: {}",
        out.raw
    );

    const MARKER: &str = "raw-callback-probe";
    assert!(
        !out.raw.contains("<script>raw-callback-probe"),
        "{label} must not contain unescaped <script from the query: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("</div><script>raw-callback-probe"),
        "{label} must not contain unescaped </div> from the query: {}",
        out.raw
    );
    assert!(
        !out.raw.contains(r#""></div><script>"#),
        "{label} must not contain an unescaped HTML/JS breakout from the query: {}",
        out.raw
    );
    assert!(
        !out.raw.contains(OAUTH_CALLBACK_ERROR_PROBE),
        "{label} must not echo the raw probe: {}",
        out.raw
    );

    let mut search = 0;
    while let Some(rel) = out.raw[search..].find(MARKER) {
        let abs = search + rel;
        let window_start = abs.saturating_sub(48);
        let window_end = (abs + MARKER.len() + 48).min(out.raw.len());
        let window = &out.raw[window_start..window_end];
        let escaped = window.contains("&lt;")
            || window.contains("&gt;")
            || window.contains("&quot;")
            || window.contains("\\u003c");
        assert!(
            escaped,
            "{label}: raw-callback-probe must only appear as an escaped form \
             (&lt; / \\u003c / &quot;), window={window:?} body={}",
            out.raw
        );
        search = abs + MARKER.len();
    }

    if expect_interpolated {
        assert!(
            out.raw.contains(MARKER),
            "{label} must interpolate error_description (escaped), not drop it: {}",
            out.raw
        );
        assert!(
            out.raw.contains("&lt;") || out.raw.contains("\\u003c") || out.raw.contains("&quot;"),
            "{label} interpolated probe must be escaped (&lt; / \\u003c / &quot;): {}",
            out.raw
        );
        let lower = out.raw.to_ascii_lowercase();
        assert!(
            lower.contains("connection failed")
                || lower.contains("could not connect")
                || lower.contains("denied"),
            "{label} popup must report connection failed: {}",
            out.raw
        );
        assert!(
            callback_reports_ok_false(&out.raw),
            "{label} popup payload must report ok=false: {}",
            out.raw
        );
    }
}

async fn assert_oauth_error_callback_not_persisted(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    label: &str,
) {
    use fleet_db::repos::integrations as integ_repo;
    use fleet_db::repos::workspaces as workspace_repo;

    let list = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "{label} GET /integrations: {}",
        list.body
    );
    for provider in ["github", "gitlab"] {
        let row = catalog_row(&list.body, provider);
        assert_eq!(
            row["connected"], false,
            "{label}: {provider} must stay disconnected: {row}"
        );
        assert!(
            row.get("connection").is_none() || row["connection"].is_null(),
            "{label}: {provider} must have no connection object: {row}"
        );
        assert!(
            credentials_for(
                &state.db,
                workspace_id,
                provider,
                &state.integrations.credentials_key
            )
            .await
            .unwrap_or_else(|e| panic!("{label}: load {provider} creds: {e}"))
            .is_none(),
            "{label}: credentials_for must not have a {provider} row"
        );
    }

    let catalog_s = list.body.to_string();
    for needle in [
        "access_token",
        "refresh_token",
        "client_secret",
        "webhook_secret",
        OAUTH_CALLBACK_ERROR_PROBE,
    ] {
        assert!(
            !catalog_s.contains(needle),
            "{label}: integrations catalog must not include {needle}: {}",
            list.body
        );
    }

    let conns = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .expect("list connections");
    assert!(
        conns
            .iter()
            .all(|c| c.provider != "github" && c.provider != "gitlab"),
        "{label}: connections must not have a github/gitlab row: {conns:?}"
    );
    let apps = integ_repo::list_oauth_apps(&state.db, workspace_id)
        .await
        .expect("list_oauth_apps");
    assert!(
        apps.iter().all(|a| a.provider != "github"),
        "{label}: oauth_apps must not have a github row: {apps:?}"
    );

    let owned = workspace_repo::list_for_user(&state.db, user_id)
        .await
        .expect("list workspaces");
    assert!(
        owned.iter().any(|w| w.id == workspace_id),
        "{label}: signup workspace must be listed: {owned:?}"
    );
    for w in &owned {
        let w_conns = integ_repo::list_all_for_workspace(&state.db, w.id)
            .await
            .expect("list connections for owned workspace");
        assert!(
            w_conns
                .iter()
                .all(|c| c.provider != "github" && c.provider != "gitlab"),
            "{label}: connections must not have a github/gitlab row for workspace {}: {w_conns:?}",
            w.id
        );
        for provider in ["github", "gitlab"] {
            assert!(
                credentials_for(
                    &state.db,
                    w.id,
                    provider,
                    &state.integrations.credentials_key
                )
                .await
                .unwrap_or_else(|e| panic!("{label}: load {provider} creds for {}: {e}", w.id))
                .is_none(),
                "{label}: credentials_for must not have a {provider} row for workspace {}",
                w.id
            );
        }
        let w_apps = integ_repo::list_oauth_apps(&state.db, w.id)
            .await
            .expect("list_oauth_apps for owned workspace");
        assert!(
            w_apps.iter().all(|a| a.provider != "github"),
            "{label}: oauth_apps must not have a github row for workspace {}: {w_apps:?}",
            w.id
        );
    }
}

/// B's cookie on A's integrations catalog / oauth/start / DELETE / PATCH defaults
/// is 403 not-a-member. Distinct from
/// [`integrations_catalog_oauth_start_delete_and_callback_contracts`] (A on A)
/// and from callback-bind / proxy-isolation / defaults-connected-only.
#[tokio::test]
async fn integrations_other_ws_catalog_oauth_start_delete_and_defaults_are_403() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let a_base = integrations_path(&a_ws);
    let b_base = integrations_path(&b_ws);
    let a_gh_start = format!("{a_base}/github/oauth/start");
    let a_gh = format!("{a_base}/github");

    let snap = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /integrations: {}",
        snap.body
    );
    assert_eq!(
        snap.body["workspace_id"].as_str(),
        Some(a_ws.as_str()),
        "snapshot workspace_id: {}",
        snap.body
    );
    let github = catalog_row(&snap.body, "github");
    assert_eq!(github["availability"], "available", "{github}");
    assert_eq!(
        github["oauth_start_path"], a_gh_start,
        "A's catalog must expose oauth_start_path so the leak check is not vacuous: {github}"
    );
    assert_eq!(
        github["connected"], false,
        "fresh workspace is disconnected so invented connection is detectable: {github}"
    );
    assert!(
        github.get("connection").is_none() || github["connection"].is_null(),
        "fresh github must have no connection object: {github}"
    );

    let nonce_before = count_oauth_state_nonces(&state.db, a_ws_id).await;

    let as_b = call(&app, "GET", &a_base, Some(&b_jar), None).await;
    assert_integrations_other_ws_forbidden(&as_b, &a_ws, &a_gh_start, "B GET A's /integrations");

    let start_as_b = call(&app, "GET", &a_gh_start, Some(&b_jar), None).await;
    assert_integrations_other_ws_forbidden(
        &start_as_b,
        &a_ws,
        &a_gh_start,
        "B GET A's /integrations/github/oauth/start",
    );
    assert_ne!(
        start_as_b.status,
        StatusCode::FOUND,
        "B oauth/start must not 302 to GitHub: {} loc={}",
        start_as_b.status,
        response_location(&start_as_b)
    );
    assert_ne!(
        start_as_b.status,
        StatusCode::TEMPORARY_REDIRECT,
        "B oauth/start must not 307 to GitHub: {} loc={}",
        start_as_b.status,
        response_location(&start_as_b)
    );
    assert_ne!(
        start_as_b.status,
        StatusCode::OK,
        "B oauth/start must not serve the App Manifest HTML: {} {}",
        start_as_b.status,
        start_as_b.raw
    );

    let del_as_b = call(&app, "DELETE", &a_gh, Some(&b_jar), None).await;
    assert_integrations_other_ws_forbidden(
        &del_as_b,
        &a_ws,
        &a_gh_start,
        "B DELETE A's /integrations/github",
    );
    assert_ne!(
        del_as_b.status,
        StatusCode::NOT_FOUND,
        "B DELETE must 403 before not-connected 404: {}",
        del_as_b.body
    );
    assert_ne!(
        del_as_b.body.get("disconnected"),
        Some(&json!(true)),
        "B DELETE must not disconnect A: {}",
        del_as_b.body
    );

    let steal_runner = format!("runner-steal-{}", Uuid::new_v4());
    let steal_model = format!("model-steal-{}", Uuid::new_v4());
    let patch_as_b = call(
        &app,
        "PATCH",
        &a_gh,
        Some(&b_jar),
        Some(json!({
            "default_runner_id": steal_runner,
            "default_provider": "ollama",
            "default_model": steal_model,
        })),
    )
    .await;
    assert_integrations_other_ws_forbidden(
        &patch_as_b,
        &a_ws,
        &a_gh_start,
        "B PATCH A's /integrations/github defaults",
    );
    let patch_hay = format!("{} {}", patch_as_b.raw, patch_as_b.body);
    assert!(
        !patch_hay.contains(&steal_runner) && !patch_hay.contains(&steal_model),
        "B PATCH 403 must not echo invented defaults: {}",
        patch_as_b.body
    );

    let nonce_after = count_oauth_state_nonces(&state.db, a_ws_id).await;
    assert_eq!(
        nonce_after, nonce_before,
        "B oauth/start must not issue/consume an oauth state nonce for A"
    );

    let after = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "A GET /integrations after B mutate: {}",
        after.body
    );
    assert_eq!(
        after.body, snap.body,
        "B GET/start/DELETE/PATCH must not change A's catalog: before={} after={}",
        snap.body, after.body
    );
    let github_after = catalog_row(&after.body, "github");
    assert_eq!(
        github_after["connected"], false,
        "A must stay disconnected (B DELETE must not invent a disconnect or a row): {github_after}"
    );
    assert!(
        github_after.get("connection").is_none() || github_after["connection"].is_null(),
        "B PATCH must not invent a connection: {github_after}"
    );
    assert_eq!(
        github_after["oauth_start_path"], a_gh_start,
        "A's oauth_start_path must survive B: {github_after}"
    );

    let a_again = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        a_again.status,
        StatusCode::OK,
        "A cookie GET /integrations must stay 200 so this is not a tautology: {}",
        a_again.body
    );

    let b_own = call(&app, "GET", &b_base, Some(&b_jar), None).await;
    assert_eq!(
        b_own.status,
        StatusCode::OK,
        "B cookie GET B /integrations must stay 200 so this is not a tautology that every integrations call is 403: {}",
        b_own.body
    );
    assert_eq!(
        b_own.body["workspace_id"].as_str(),
        Some(b_ws.as_str()),
        "B catalog is B's workspace: {}",
        b_own.body
    );
    let b_github = catalog_row(&b_own.body, "github");
    assert_eq!(b_github["availability"], "available", "{b_github}");
    assert_eq!(
        b_github["oauth_start_path"],
        format!("{b_base}/github/oauth/start"),
        "B's catalog must expose B's oauth_start_path: {b_github}"
    );
}

fn assert_integrations_other_ws_forbidden(
    out: &HttpOut,
    a_ws: &str,
    a_oauth_start_path: &str,
    label: &str,
) {
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    if let Some(arr) = out.body.get("integrations").and_then(|v| v.as_array()) {
        assert!(
            arr.is_empty(),
            "{label} must not include A's catalog rows: {}",
            out.body
        );
    }
    if let Some(ws) = out.body.get("workspace_id") {
        assert_ne!(
            ws.as_str(),
            Some(a_ws),
            "{label} must not echo A's workspace_id: {}",
            out.body
        );
    }
    let keys = json_object_keys(&out.body);
    for key in [
        "oauth_start_path",
        "connected",
        "connection",
        "access_token",
        "refresh_token",
        "client_secret",
        "disconnected",
    ] {
        assert!(
            !keys.iter().any(|k| k == key),
            "{label} must not include {key}: {}",
            out.body
        );
    }
    let hay = format!("{} {}", out.raw, out.body);
    for needle in [
        "access_token",
        "refresh_token",
        "client_secret",
        "oauth_start_path",
        a_oauth_start_path,
        "github.com/settings/apps/new",
        "name=\"manifest\"",
    ] {
        assert!(
            !hay.contains(needle),
            "{label} must not include {needle:?}: {}",
            out.raw
        );
    }
    let loc = response_location(out);
    let loc_l = loc.to_ascii_lowercase();
    assert!(
        loc.is_empty()
            || !(loc_l.contains("github.com")
                || loc_l.contains("gitlab.com")
                || loc.contains("state=")),
        "{label} must not 302 to GitHub or issue oauth state: {loc}"
    );
    let issued = issued_oauth_state_jwts(out);
    assert!(
        issued.is_empty(),
        "{label} must not issue an oauth state nonce JWT: {issued:?} body={}",
        out.body
    );
}

/// Fleet holds the app client id/secret; users never see tokens.
/// PUT `/integrations/{provider}/oauth-app` and POST `/oauth/start` (bootstrap
/// form) persist an encrypted envelope. Response JSON, GET /integrations, and
/// GET oauth/start after save must not contain the secret string or a plaintext
/// `client_secret` field. Distinct from oauth-start-unconfigured (GET start
/// still bootstraps when unset), token-paste-not-accepted (no PAT body), and
/// oauth-callback-binds-workspace.
#[tokio::test]
async fn oauth_app_put_and_start_form_never_return_client_secret() {
    use fleet_db::repos::integrations as integ_repo;

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let b_ws_id = Uuid::parse_str(&b_ws).expect("B workspace id");
    let a_base = integrations_path(&a_ws);
    let b_base = integrations_path(&b_ws);

    let a_apps0 = integ_repo::list_oauth_apps(&state.db, a_ws_id)
        .await
        .expect("A list_oauth_apps before");
    let b_apps0 = integ_repo::list_oauth_apps(&state.db, b_ws_id)
        .await
        .expect("B list_oauth_apps before");
    assert!(
        a_apps0.is_empty() && b_apps0.is_empty(),
        "fresh workspaces must have no oauth_apps so persist checks are not vacuous: A={a_apps0:?} B={b_apps0:?}"
    );

    let tag = Uuid::now_v7();
    let put_id = format!("gh-oauth-app-{tag}");
    let put_secret = format!("put_oauth_app_secret_{tag}");
    let form_id = format!("gl-oauth-app-{tag}");
    let form_secret = format!("post_oauth_start_secret_{tag}");
    let steal_secret = format!("steal_oauth_app_secret_{tag}");

    // Empty / whitespace client_id or client_secret → 400, no persist.
    for (label, body) in [
        (
            "PUT empty client_id",
            json!({ "client_id": "", "client_secret": put_secret }),
        ),
        (
            "PUT empty client_secret",
            json!({ "client_id": put_id, "client_secret": "" }),
        ),
        (
            "PUT whitespace id and secret",
            json!({ "client_id": "   ", "client_secret": "\t  " }),
        ),
    ] {
        let out = call(
            &app,
            "PUT",
            &format!("{a_base}/github/oauth-app"),
            Some(&a_jar),
            Some(body),
        )
        .await;
        assert_oauth_app_client_error(&out, StatusCode::BAD_REQUEST, &put_secret, label);
        assert_no_oauth_apps(&state.db, a_ws_id, "A after empty PUT").await;
    }
    for (label, form) in [
        (
            "POST empty client_id",
            format!("client_id=&client_secret={form_secret}"),
        ),
        (
            "POST empty client_secret",
            format!("client_id={form_id}&client_secret="),
        ),
        (
            "POST whitespace id and secret",
            "client_id=+++&client_secret=+++".to_string(),
        ),
    ] {
        let out = call_form(
            &app,
            "POST",
            &format!("{a_base}/gitlab/oauth/start"),
            Some(&a_jar),
            &form,
        )
        .await;
        assert_oauth_app_client_error(&out, StatusCode::BAD_REQUEST, &form_secret, label);
        assert_no_oauth_apps(&state.db, a_ws_id, "A after empty POST oauth/start").await;
    }

    // coming_soon (bitbucket/confluence) → 400 and must not persist.
    for provider in ["bitbucket", "confluence"] {
        let out = call(
            &app,
            "PUT",
            &format!("{a_base}/{provider}/oauth-app"),
            Some(&a_jar),
            Some(json!({
                "client_id": format!("{provider}-{tag}"),
                "client_secret": put_secret,
            })),
        )
        .await;
        assert_oauth_app_client_error(
            &out,
            StatusCode::BAD_REQUEST,
            &put_secret,
            &format!("PUT {provider} oauth-app"),
        );
        let err = http_error_text(&out).to_ascii_lowercase();
        assert!(
            err.contains("not available"),
            "coming_soon {provider} PUT must say not available: {}",
            out.body
        );
        let start = call_form(
            &app,
            "POST",
            &format!("{a_base}/{provider}/oauth/start"),
            Some(&a_jar),
            &format!("client_id={provider}-{tag}&client_secret={put_secret}"),
        )
        .await;
        assert_oauth_app_client_error(
            &start,
            StatusCode::BAD_REQUEST,
            &put_secret,
            &format!("POST {provider} oauth/start"),
        );
        assert!(
            integ_repo::get_oauth_app(&state.db, a_ws_id, provider)
                .await
                .expect("get coming_soon oauth_app")
                .is_none(),
            "coming_soon {provider} must not persist an oauth_apps row"
        );
    }
    assert_no_oauth_apps(&state.db, a_ws_id, "A after coming_soon rejects").await;

    // Unknown provider → 404, no persist.
    let unknown_put = call(
        &app,
        "PUT",
        &format!("{a_base}/notaprovider/oauth-app"),
        Some(&a_jar),
        Some(json!({
            "client_id": format!("unknown-{tag}"),
            "client_secret": put_secret,
        })),
    )
    .await;
    assert_oauth_app_client_error(
        &unknown_put,
        StatusCode::NOT_FOUND,
        &put_secret,
        "PUT unknown oauth-app",
    );
    let unknown_post = call_form(
        &app,
        "POST",
        &format!("{a_base}/notaprovider/oauth/start"),
        Some(&a_jar),
        &format!("client_id=unknown-{tag}&client_secret={put_secret}"),
    )
    .await;
    assert_oauth_app_client_error(
        &unknown_post,
        StatusCode::NOT_FOUND,
        &put_secret,
        "POST unknown oauth/start",
    );
    assert_no_oauth_apps(&state.db, a_ws_id, "A after unknown provider").await;

    // Other-ws cookie is 403 and must not write an oauth_apps row on B (or A).
    let steal_put = call(
        &app,
        "PUT",
        &format!("{a_base}/github/oauth-app"),
        Some(&b_jar),
        Some(json!({
            "client_id": format!("steal-{tag}"),
            "client_secret": steal_secret,
        })),
    )
    .await;
    assert_eq!(
        steal_put.status,
        StatusCode::FORBIDDEN,
        "B PUT A's oauth-app must be 403: {} {}",
        steal_put.status,
        steal_put.body
    );
    let steal_err = http_error_text(&steal_put).to_ascii_lowercase();
    assert!(
        steal_err.contains("not a member"),
        "other-ws PUT 403 must say not a member: {}",
        steal_put.body
    );
    assert_oauth_app_response_omits_secret(&steal_put, &steal_secret, "B PUT A's oauth-app");
    let steal_post = call_form(
        &app,
        "POST",
        &format!("{a_base}/gitlab/oauth/start"),
        Some(&b_jar),
        &format!("client_id=steal-{tag}&client_secret={steal_secret}"),
    )
    .await;
    assert_eq!(
        steal_post.status,
        StatusCode::FORBIDDEN,
        "B POST A's oauth/start must be 403: {} {}",
        steal_post.status,
        steal_post.body
    );
    assert_oauth_app_response_omits_secret(&steal_post, &steal_secret, "B POST A's oauth/start");
    assert_no_oauth_apps(&state.db, a_ws_id, "A after other-ws steal").await;
    assert_no_oauth_apps(&state.db, b_ws_id, "B after other-ws steal").await;

    // Owner PUT persists an envelope; HTTP JSON must not echo the secret.
    let saved = call(
        &app,
        "PUT",
        &format!("{a_base}/github/oauth-app"),
        Some(&a_jar),
        Some(json!({
            "client_id": put_id,
            "client_secret": put_secret,
        })),
    )
    .await;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "owner PUT github oauth-app: {}",
        saved.body
    );
    assert_eq!(saved.body["provider"], "github", "{}", saved.body);
    assert_eq!(saved.body["configured"], true, "{}", saved.body);
    assert_eq!(
        saved.body["oauth_start_path"],
        format!("{a_base}/github/oauth/start"),
        "{}",
        saved.body
    );
    assert_oauth_app_response_omits_secret(&saved, &put_secret, "PUT github oauth-app");
    assert_stored_oauth_app_envelope(
        &state,
        a_ws_id,
        "github",
        &put_id,
        &put_secret,
        "PUT github",
    )
    .await;

    let catalog = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        catalog.status,
        StatusCode::OK,
        "GET /integrations after PUT: {}",
        catalog.body
    );
    assert_eq!(
        catalog_row(&catalog.body, "github")["oauth_configured"],
        true,
        "PUT must mark github oauth_configured so the omit check is not vacuous: {}",
        catalog.body
    );
    assert_oauth_app_response_omits_secret(&catalog, &put_secret, "GET /integrations after PUT");
    assert_oauth_app_response_omits_secret(&catalog, &form_secret, "GET /integrations after PUT");

    let start_after_put = call(
        &app,
        "GET",
        &format!("{a_base}/github/oauth/start"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_oauth_start_redirects_to_api_public_base(
        &start_after_put,
        "github",
        "GET github oauth/start after owner PUT oauth-app",
    );
    assert_oauth_app_response_omits_secret(
        &start_after_put,
        &put_secret,
        "GET github oauth/start after PUT",
    );
    let start_html = start_after_put.raw.to_ascii_lowercase();
    assert!(
        !start_html.contains(&format!("value=\"{put_secret}\""))
            && !start_html.contains(&format!("value='{put_secret}'")),
        "GET oauth/start HTML after save must not prefill the secret: {}",
        start_after_put.raw
    );

    // Owner POST bootstrap form also persists; 302/307 must not return the secret.
    let posted = call_form(
        &app,
        "POST",
        &format!("{a_base}/gitlab/oauth/start"),
        Some(&a_jar),
        &format!("client_id={form_id}&client_secret={form_secret}"),
    )
    .await;
    assert_oauth_start_redirects_to_api_public_base(
        &posted,
        "gitlab",
        "owner POST gitlab oauth/start form after persist",
    );
    assert_oauth_app_response_omits_secret(&posted, &form_secret, "POST gitlab oauth/start");
    assert_stored_oauth_app_envelope(
        &state,
        a_ws_id,
        "gitlab",
        &form_id,
        &form_secret,
        "POST gitlab oauth/start",
    )
    .await;

    let catalog2 = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        catalog2.status,
        StatusCode::OK,
        "GET /integrations after POST form: {}",
        catalog2.body
    );
    assert_eq!(
        catalog_row(&catalog2.body, "gitlab")["oauth_configured"],
        true,
        "POST form must mark gitlab oauth_configured: {}",
        catalog2.body
    );
    assert_oauth_app_response_omits_secret(
        &catalog2,
        &form_secret,
        "GET /integrations after POST form",
    );
    assert_oauth_app_response_omits_secret(
        &catalog2,
        &put_secret,
        "GET /integrations after POST form (github secret)",
    );

    let start_after_post = call(
        &app,
        "GET",
        &format!("{a_base}/gitlab/oauth/start"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_oauth_start_redirects_to_api_public_base(
        &start_after_post,
        "gitlab",
        "GET gitlab oauth/start after POST form",
    );
    assert_oauth_app_response_omits_secret(
        &start_after_post,
        &form_secret,
        "GET gitlab oauth/start after POST form",
    );

    // Control: B's own catalog is 200 and B still has no oauth_apps / no A's secrets.
    let b_own = call(&app, "GET", &b_base, Some(&b_jar), None).await;
    assert_eq!(
        b_own.status,
        StatusCode::OK,
        "B GET B /integrations must stay 200 so this is not a tautology: {}",
        b_own.body
    );
    assert_oauth_app_response_omits_secret(&b_own, &put_secret, "B GET B /integrations");
    assert_oauth_app_response_omits_secret(&b_own, &form_secret, "B GET B /integrations");
    assert_no_oauth_apps(&state.db, b_ws_id, "B after A saved oauth-apps").await;
}

/// Owner PUT github oauth-app must persist an AES-GCM envelope at rest.
/// `decrypt()` of a raw `{client_secret}` blob still succeeds, so HTTP omit
/// plus decrypt-control is not enough — SELECT `client_secret::text`.
/// Other-ws stays 403; coming_soon PUT stays 400 and inserts no row.
#[tokio::test]
async fn oauth_app_credentials_encrypted_at_rest() {
    use fleet_db::repos::integrations as integ_repo;

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let b_ws_id = Uuid::parse_str(&b_ws).expect("B workspace id");
    let a_base = integrations_path(&a_ws);

    assert_no_oauth_apps(&state.db, a_ws_id, "A before PUT").await;
    assert_no_oauth_apps(&state.db, b_ws_id, "B before PUT").await;

    let tag = Uuid::now_v7();
    let put_id = format!("gh-enc-oauth-app-{tag}");
    let put_secret = format!("put_oauth_app_enc_secret_{tag}");
    let steal_secret = format!("steal_oauth_app_enc_secret_{tag}");

    // Other-ws cookie is 403 and must not write an oauth_apps row on B (or A).
    let steal_put = call(
        &app,
        "PUT",
        &format!("{a_base}/github/oauth-app"),
        Some(&b_jar),
        Some(json!({
            "client_id": format!("steal-enc-{tag}"),
            "client_secret": steal_secret,
        })),
    )
    .await;
    assert_eq!(
        steal_put.status,
        StatusCode::FORBIDDEN,
        "B PUT A's oauth-app must be 403: {} {}",
        steal_put.status,
        steal_put.body
    );
    let steal_err = http_error_text(&steal_put).to_ascii_lowercase();
    assert!(
        steal_err.contains("not a member"),
        "other-ws PUT 403 must say not a member: {}",
        steal_put.body
    );
    assert_oauth_app_response_omits_secret(&steal_put, &steal_secret, "B PUT A's oauth-app");
    assert_no_oauth_apps(&state.db, a_ws_id, "A after other-ws steal").await;
    assert_no_oauth_apps(&state.db, b_ws_id, "B after other-ws steal").await;

    // coming_soon PUT still 400 and must not insert an oauth_apps row.
    for provider in ["bitbucket", "confluence"] {
        let out = call(
            &app,
            "PUT",
            &format!("{a_base}/{provider}/oauth-app"),
            Some(&a_jar),
            Some(json!({
                "client_id": format!("{provider}-enc-{tag}"),
                "client_secret": put_secret,
            })),
        )
        .await;
        assert_oauth_app_client_error(
            &out,
            StatusCode::BAD_REQUEST,
            &put_secret,
            &format!("PUT {provider} oauth-app"),
        );
        let err = http_error_text(&out).to_ascii_lowercase();
        assert!(
            err.contains("not available"),
            "coming_soon {provider} PUT must say not available: {}",
            out.body
        );
        assert!(
            integ_repo::get_oauth_app(&state.db, a_ws_id, provider)
                .await
                .expect("get coming_soon oauth_app")
                .is_none(),
            "coming_soon {provider} must not persist an oauth_apps row"
        );
    }
    assert_no_oauth_apps(&state.db, a_ws_id, "A after coming_soon rejects").await;

    // Owner PUT persists an envelope; HTTP JSON must not echo the secret.
    let saved = call(
        &app,
        "PUT",
        &format!("{a_base}/github/oauth-app"),
        Some(&a_jar),
        Some(json!({
            "client_id": put_id,
            "client_secret": put_secret,
        })),
    )
    .await;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "owner PUT github oauth-app: {}",
        saved.body
    );
    assert_eq!(saved.body["provider"], "github", "{}", saved.body);
    assert_eq!(saved.body["configured"], true, "{}", saved.body);
    assert_oauth_app_response_omits_secret(&saved, &put_secret, "PUT github oauth-app");
    assert_stored_oauth_app_envelope(
        &state,
        a_ws_id,
        "github",
        &put_id,
        &put_secret,
        "PUT github",
    )
    .await;

    let catalog = call(&app, "GET", &a_base, Some(&a_jar), None).await;
    assert_eq!(
        catalog.status,
        StatusCode::OK,
        "GET /integrations after PUT: {}",
        catalog.body
    );
    assert_eq!(
        catalog_row(&catalog.body, "github")["oauth_configured"],
        true,
        "PUT must mark github oauth_configured so the omit check is not vacuous: {}",
        catalog.body
    );
    assert_oauth_app_response_omits_secret(&catalog, &put_secret, "GET /integrations after PUT");
    let catalog_keys = json_object_keys(&catalog.body);
    for forbidden in ["client_secret", "list_oauth_apps", "oauth_apps"] {
        assert!(
            !catalog_keys.iter().any(|k| k == forbidden),
            "GET /integrations must not return {forbidden} as a secret field: {}",
            catalog.body
        );
    }
    assert!(
        !catalog.raw.contains(&put_secret),
        "GET /integrations must not contain the planted secret: {}",
        catalog.raw
    );

    assert_no_oauth_apps(&state.db, b_ws_id, "B after A saved oauth-app").await;
}

fn assert_oauth_app_client_error(out: &HttpOut, want: StatusCode, secret: &str, label: &str) {
    assert_eq!(
        out.status, want,
        "{label} must be {want}: {} {}",
        out.status, out.body
    );
    assert!(
        !out.status.is_success() && !out.status.is_server_error(),
        "{label} must be 4xx, got {} {}",
        out.status,
        out.body
    );
    assert_oauth_app_response_omits_secret(out, secret, label);
}

fn assert_oauth_app_response_omits_secret(out: &HttpOut, secret: &str, label: &str) {
    let keys = json_object_keys(&out.body);
    assert!(
        !keys.iter().any(|k| k == "client_secret"),
        "{label} must not include a plaintext client_secret field: {}",
        out.body
    );
    let loc = response_location(out);
    let set_cookies = parse_set_cookies(&out.headers)
        .into_iter()
        .map(|c| c.raw)
        .collect::<Vec<_>>()
        .join(" ");
    let hay = format!("{} {} {} {}", out.raw, out.body, loc, set_cookies);
    assert!(
        !hay.contains(secret),
        "{label} must not contain the client_secret string: {}",
        out.raw
    );
}

async fn assert_no_oauth_apps(db: &fleet_db::PgPool, workspace_id: Uuid, label: &str) {
    let apps = fleet_db::repos::integrations::list_oauth_apps(db, workspace_id)
        .await
        .expect("list_oauth_apps");
    assert!(
        apps.is_empty(),
        "{label} must not persist oauth_apps: {apps:?}"
    );
}

async fn assert_stored_oauth_app_envelope(
    state: &fleet_server::AppState,
    workspace_id: Uuid,
    provider: &str,
    client_id: &str,
    secret: &str,
    label: &str,
) {
    let app = fleet_db::repos::integrations::get_oauth_app(&state.db, workspace_id, provider)
        .await
        .expect("get_oauth_app")
        .unwrap_or_else(|| panic!("{label}: persist must write an oauth_apps row"));
    assert_eq!(app.client_id, client_id, "{label} stored client_id");
    assert_value_is_aes_gcm_envelope(
        &app.client_secret,
        secret,
        &format!("{label} get_oauth_app"),
    );

    let raw_text: String = sqlx::query_scalar(
        r#"
        SELECT client_secret::text
        FROM integration_oauth_apps
        WHERE workspace_id = $1 AND provider = $2
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .fetch_one(&state.db)
    .await
    .unwrap_or_else(|e| panic!("{label}: SELECT client_secret::text: {e}"));
    assert!(
        !raw_text.contains(secret),
        "{label}: raw client_secret::text must not contain the plaintext secret: {raw_text}"
    );
    assert!(
        !raw_text.contains("\"client_secret\"") && !raw_text.contains("client_secret"),
        "{label}: raw jsonb/text must not contain a plaintext client_secret key: {raw_text}"
    );
    let raw_json: Value = serde_json::from_str(&raw_text)
        .unwrap_or_else(|e| panic!("{label}: client_secret::text must be JSON: {e}: {raw_text}"));
    assert_value_is_aes_gcm_envelope(&raw_json, secret, &format!("{label} client_secret::text"));

    let plain = state
        .integrations
        .credentials_key
        .decrypt(&app.client_secret)
        .unwrap_or_else(|e| panic!("{label}: decrypt stored envelope: {e}"));
    assert_eq!(
        plain.get("client_secret").and_then(|v| v.as_str()),
        Some(secret),
        "{label}: decrypt control — persist must have sealed this secret: {plain}"
    );
}

/// AES-256-GCM envelope written by `CredentialsKey::encrypt`. decrypt() of a
/// raw `{client_secret}` blob still succeeds — this shape is the at-rest lock.
fn assert_value_is_aes_gcm_envelope(stored: &Value, plaintext: &str, label: &str) {
    assert_eq!(
        stored.get("_enc").and_then(|v| v.as_str()),
        Some("v1"),
        "{label}: stored jsonb must be an AES-GCM envelope (_enc=v1), not plaintext: {stored}"
    );
    let nonce = stored
        .get("nonce")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}: envelope missing nonce: {stored}"));
    let ct = stored
        .get("ct")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}: envelope missing ct: {stored}"));
    assert_eq!(
        nonce.len(),
        24,
        "{label}: nonce must be 12-byte hex (24 chars): {nonce}"
    );
    assert!(
        nonce.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: nonce must be hex: {nonce}"
    );
    assert!(
        ct.len() >= 32 && ct.len() % 2 == 0 && ct.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: ct must be non-empty even hex (ciphertext+tag): {ct}"
    );
    let obj = stored
        .as_object()
        .unwrap_or_else(|| panic!("{label}: envelope must be a JSON object: {stored}"));
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["_enc", "nonce", "ct"].into_iter().collect(),
        "{label}: envelope keys must be _enc/nonce/ct only (no client_secret): {stored}"
    );
    let raw = stored.to_string();
    assert!(
        !raw.contains(plaintext),
        "{label}: stored jsonb must not contain the plaintext secret: {raw}"
    );
}

/// Product Trello Connect: callback `code` is the user token (no live exchange).
async fn connect_trello_http(
    app: &axum::Router,
    jar: &CookieJar,
    user_id: Uuid,
    workspace_id: Uuid,
    access_token: &str,
) {
    let state_jwt =
        issue_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "trello").expect("trello state");
    let claims = verify_oauth_state(TEST_JWT_SECRET, &state_jwt).expect("issued state verifies");
    assert_eq!(claims.provider, "trello");
    assert_eq!(claims.workspace_id, workspace_id.to_string());
    let connected = call(
        app,
        "GET",
        &format!("/api/integrations/oauth/callback/trello?code={access_token}&state={state_jwt}"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(
        connected.status,
        StatusCode::OK,
        "trello Connect via oauth_callback: {} {}",
        connected.status,
        connected.raw
    );
    let lower = connected.raw.to_ascii_lowercase();
    assert!(
        lower.contains("ok: true") || lower.contains("ok:true"),
        "trello Connect must popup-close as connected: {}",
        connected.raw
    );
    assert!(
        !callback_reports_ok_false(&connected.raw),
        "trello Connect must not report ok:false: {}",
        connected.raw
    );
}

/// HTTP DELETE of a connected Trello row must wipe credentials and stop the
/// proxy. Catalog never-connected 404s stay in the contract test above.
#[tokio::test]
async fn http_disconnect_connected_trello_wipes_credentials_and_stops_proxy() {
    use fleet_db::repos::integrations as integ_repo;

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace uuid");
    let a_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(a_me.status, StatusCode::OK, "A me: {}", a_me.body);
    let a_user = Uuid::parse_str(
        a_me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("A user.id: {}", a_me.body)),
    )
    .expect("A user id");

    let tag = Uuid::now_v7();
    let access = format!("tok_http_disconnect_{tag}");
    let reconnect = format!("tok_http_disconnect_re_{tag}");
    let client_secret = format!("trello_client_secret_{tag}");

    let saved = call(
        &app,
        "PUT",
        &format!("/api/workspaces/{a_ws}/integrations/trello/oauth-app"),
        Some(&a_jar),
        Some(json!({
            "client_id": format!("trello-http-disconnect-{tag}"),
            "client_secret": client_secret,
        })),
    )
    .await;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "put trello oauth-app: {}",
        saved.body
    );

    connect_trello_http(&app, &a_jar, a_user, a_ws_id, &access).await;

    let before = credentials_for(
        &state.db,
        a_ws_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("load trello creds before")
    .expect("connected trello must store credentials (control — not a tautology)");
    assert_eq!(before["access_token"], access);
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "trello")
            .await
            .expect("find_connected before")
            .is_some(),
        "connected trello row must exist before DELETE"
    );
    let catalog_before = call(
        &app,
        "GET",
        &format!("/api/workspaces/{a_ws}/integrations"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        catalog_before.status,
        StatusCode::OK,
        "GET /integrations before: {}",
        catalog_before.body
    );
    assert_eq!(
        catalog_row(&catalog_before.body, "trello")["connected"],
        true,
        "trello must show connected before DELETE: {}",
        catalog_before.body
    );

    let steal = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{a_ws}/integrations/trello"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        steal.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A DELETE must be 403: {}",
        steal.body
    );
    let steal_err = steal.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        steal_err.contains("not a member"),
        "other-ws 403 must say not a member: {}",
        steal.body
    );
    let steal_hay = format!("{} {}", steal.raw, steal.body);
    for needle in [
        "access_token",
        "refresh_token",
        "client_secret",
        "_enc",
        access.as_str(),
        client_secret.as_str(),
    ] {
        assert!(
            !steal_hay.contains(needle),
            "other-ws 403 must not contain {needle}: {}",
            steal.raw
        );
    }
    let still = credentials_for(
        &state.db,
        a_ws_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("load trello after other-ws")
    .expect("other-ws DELETE must not wipe A");
    assert_eq!(still["access_token"], access);
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "trello")
            .await
            .expect("find_connected after other-ws")
            .is_some(),
        "other-ws DELETE must leave A connected"
    );

    let disc = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{a_ws}/integrations/trello"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        disc.status,
        StatusCode::OK,
        "owner DELETE connected trello: {}",
        disc.body
    );
    assert_eq!(disc.body["disconnected"], true, "{}", disc.body);
    assert_eq!(disc.body["provider"], "trello", "{}", disc.body);
    let disc_obj = disc
        .body
        .as_object()
        .unwrap_or_else(|| panic!("DELETE body object: {}", disc.body));
    for key in [
        "access_token",
        "refresh_token",
        "client_secret",
        "credentials",
        "_enc",
        "ct",
    ] {
        assert!(
            !disc_obj.contains_key(key),
            "DELETE body must not include {key}: {}",
            disc.body
        );
    }
    let disc_hay = format!("{} {}", disc.raw, disc.body);
    for needle in [
        "access_token",
        "refresh_token",
        "client_secret",
        "_enc",
        access.as_str(),
        client_secret.as_str(),
    ] {
        assert!(
            !disc_hay.contains(needle),
            "DELETE body must not contain {needle}: {}",
            disc.raw
        );
    }

    assert!(
        credentials_for(
            &state.db,
            a_ws_id,
            "trello",
            &state.integrations.credentials_key
        )
        .await
        .expect("load trello after DELETE")
        .is_none(),
        "credentials_for must be None after HTTP disconnect"
    );
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "trello")
            .await
            .expect("find_connected after DELETE")
            .is_none(),
        "find_connected must be None after HTTP disconnect"
    );
    for row in integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("list after DELETE")
        .iter()
        .filter(|c| c.provider == "trello")
    {
        assert_eq!(row.status, "disconnected", "leftover trello row: {row:?}");
        if let Ok(plain) = state.integrations.credentials_key.decrypt(&row.credentials) {
            let dumped = plain.to_string();
            assert!(
                !dumped.contains(&access),
                "leftover credentials must not still hold the unique token: {plain}"
            );
            assert_ne!(
                plain.get("access_token").and_then(|v| v.as_str()),
                Some(access.as_str()),
                "leftover access_token must be wiped: {plain}"
            );
        }
    }

    let catalog_after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{a_ws}/integrations"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        catalog_after.status,
        StatusCode::OK,
        "GET /integrations after: {}",
        catalog_after.body
    );
    assert_eq!(
        catalog_row(&catalog_after.body, "trello")["connected"],
        false,
        "GET /integrations trello must not be connected: {}",
        catalog_after.body
    );
    let catalog_hay = format!("{} {}", catalog_after.raw, catalog_after.body);
    assert!(
        !catalog_hay.contains(&access) && !catalog_hay.contains(&client_secret),
        "catalog must not echo the unique token: {}",
        catalog_after.body
    );

    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };
    let (content, action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        a_user,
        "integration_query",
        &json!({
            "provider": "trello",
            "operation": "list_boards",
        }),
        &llm,
    )
    .await;
    assert!(
        !action.ok,
        "integration_query list_boards after DELETE must be ok:false: action={action:?} content={content}"
    );
    assert_eq!(action.tool, "integration_query");
    assert_eq!(
        content["ok"], false,
        "integration_query body must be ok:false: {content}"
    );
    let err_code = content["error_code"].as_str().unwrap_or("");
    assert_eq!(
        err_code, "not_connected",
        "disconnected list_boards must be not_connected (must not call upstream): {content}"
    );
    assert!(
        content["provider_status"].is_null(),
        "not_connected must not carry an upstream provider_status: {content}"
    );
    let tool_hay = format!("{content} {action:?}");
    for needle in [
        access.as_str(),
        client_secret.as_str(),
        "access_token",
        "refresh_token",
        "trello.com",
        "api.trello.com",
        "Trello API",
    ] {
        assert!(
            !tool_hay.contains(needle),
            "integration_query after DELETE must not open trello.com or echo {needle}: {tool_hay}"
        );
    }

    let again = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{a_ws}/integrations/trello"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::NOT_FOUND,
        "second DELETE must stay 404 not connected: {}",
        again.body
    );
    let again_err = again.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        again_err.contains("not connected"),
        "second DELETE must say not connected: {}",
        again.body
    );

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every request 404s: {}",
        health.body
    );

    connect_trello_http(&app, &a_jar, a_user, a_ws_id, &reconnect).await;
    let after_re = credentials_for(
        &state.db,
        a_ws_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("load trello after re-Connect")
    .expect("fresh nonce after DELETE may complete Connect");
    assert_eq!(after_re["access_token"], reconnect);
    assert_ne!(after_re["access_token"], access);
}

/// Public GET/POST /api/integrations/webhooks/github must not upsert a
/// connection from an unsigned (or junk-signed) push/installation event,
/// and must not echo webhook_secret / pem / client_secret / tokens.
/// A no-op 204 is fine; persist is not.
#[tokio::test]
async fn github_webhook_unsigned_does_not_persist_connection_or_return_secrets() {
    use fleet_db::repos::integrations as integ_repo;
    use fleet_db::repos::workspaces as workspace_repo;

    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");

    let owned_before = workspace_repo::list_for_user(&state.db, user_id)
        .await
        .expect("list workspaces before");
    assert!(
        owned_before.iter().any(|w| w.id == workspace_id),
        "signup workspace must be listed for persist check: {owned_before:?}"
    );
    let conns_before = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .expect("list connections before");
    assert!(
        conns_before.iter().all(|c| c.provider != "github"),
        "fresh workspace must have no github connection so persist check is not vacuous: {conns_before:?}"
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github creds before")
        .is_none(),
        "fresh workspace must have no github credentials"
    );

    const PATH: &str = "/api/integrations/webhooks/github";

    // 1. Unauthenticated GET and POST (empty body).
    let get_out = call(&app, "GET", PATH, None, None).await;
    assert_github_webhook_unsigned_closed(&get_out, "anon GET webhook");
    let post_empty = call(&app, "POST", PATH, None, None).await;
    assert_github_webhook_unsigned_closed(&post_empty, "anon POST webhook empty body");

    // Installation / push-shaped bodies with a made-up action. workspace_id is
    // included so a handler that upserts from the JSON would hit this row.
    let installation = json!({
        "action": "unsigned-test-created",
        "installation": {
            "id": 424242,
            "account": { "login": "unsigned-hook" }
        },
        "sender": { "login": "unsigned-hook" },
        "workspace_id": workspace_id,
        "user_id": user_id,
    });
    let post_install = call(&app, "POST", PATH, None, Some(installation)).await;
    assert_github_webhook_unsigned_closed(
        &post_install,
        "anon POST webhook installation-shaped body",
    );

    let push = json!({
        "action": "unsigned-test-push",
        "ref": "refs/heads/main",
        "repository": { "full_name": "unsigned-hook/repo" },
        "installation": { "id": 424242 },
        "commits": [],
        "workspace_id": workspace_id,
        "user_id": user_id,
    });
    let post_push = call(&app, "POST", PATH, None, Some(push.clone())).await;
    assert_github_webhook_unsigned_closed(&post_push, "anon POST webhook push-shaped body");

    assert_github_webhook_not_persisted(&app, &state, &jar, &ws, workspace_id, user_id).await;

    // 3. Junk X-Hub-Signature-256 must not become a success path that upserts.
    let junk_sig = call_headers(
        &app,
        "POST",
        PATH,
        &[("X-Hub-Signature-256", "sha256=deadbeef")],
        Some(push),
    )
    .await;
    assert_github_webhook_unsigned_closed(&junk_sig, "anon POST webhook junk X-Hub-Signature-256");

    assert_github_webhook_not_persisted(&app, &state, &jar, &ws, workspace_id, user_id).await;

    // 4. Control: /health still 200 (not every unauthenticated POST 404s the app).
    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every unauthenticated POST 404s: {}",
        health.body
    );
}

/// Public GitHub App Manifest callback must not INSERT oauth_apps or echo
/// pem / webhook_secret / client_secret. A leftover user JWT or owner
/// cookie is not enough without a real GitHub convert code.
#[tokio::test]
async fn github_manifest_callback_does_not_persist_oauth_apps_or_leak_pem_secret() {
    use fleet_db::repos::integrations as integ_repo;

    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");

    let apps_before = integ_repo::list_oauth_apps(&state.db, workspace_id)
        .await
        .expect("list_oauth_apps before");
    assert!(
        apps_before.is_empty(),
        "fresh workspace must have no oauth_apps so persist check is not vacuous: {apps_before:?}"
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github creds before")
        .is_none(),
        "fresh workspace must have no github credentials"
    );

    // 1. Unauthenticated GET — no query, empty code, empty state.
    for (label, uri) in [
        (
            "anon GET manifest/callback (no query)",
            github_manifest_callback_uri(None, None),
        ),
        (
            "anon GET manifest/callback (empty code)",
            github_manifest_callback_uri(Some(""), None),
        ),
        (
            "anon GET manifest/callback (empty state)",
            github_manifest_callback_uri(None, Some("")),
        ),
        (
            "anon GET manifest/callback (empty code+state)",
            github_manifest_callback_uri(Some(""), Some("")),
        ),
    ] {
        let out = call(&app, "GET", &uri, None, None).await;
        assert_github_manifest_callback_closed(&out, label);
    }

    // 2. Forged / access / expired / GitLab state + fake-code must not persist.
    let forged = forged_oauth_state(user_id, workspace_id, "github");
    assert!(
        verify_oauth_state(TEST_JWT_SECRET, &forged).is_err(),
        "forged JWT must not verify as oauth state"
    );
    let access = jar.access().expect("login issued fleet_access").to_string();
    assert!(
        verify_oauth_state(TEST_JWT_SECRET, &access).is_err(),
        "fleet_access JWT must not verify as oauth state"
    );
    let expired = expired_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "github");
    assert!(
        verify_oauth_state(TEST_JWT_SECRET, &expired).is_err(),
        "expired JWT must not verify as oauth state"
    );
    let gitlab_state =
        issue_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "gitlab").expect("gitlab state");
    let gitlab_claims = verify_oauth_state(TEST_JWT_SECRET, &gitlab_state)
        .expect("gitlab issue_oauth_state verifies");
    assert_eq!(gitlab_claims.provider, "gitlab");
    assert_ne!(gitlab_claims.provider, "github");

    for (label, state_jwt) in [
        ("anon GET manifest/callback (forged JWT)", forged.as_str()),
        (
            "anon GET manifest/callback (fleet_access JWT)",
            access.as_str(),
        ),
        ("anon GET manifest/callback (expired JWT)", expired.as_str()),
        (
            "anon GET manifest/callback (gitlab issue_oauth_state)",
            gitlab_state.as_str(),
        ),
    ] {
        let out = call(
            &app,
            "GET",
            &github_manifest_callback_uri(Some("fake-code"), Some(state_jwt)),
            None,
            None,
        )
        .await;
        assert_github_manifest_callback_closed(&out, label);
    }

    assert_github_not_stored_oauth_app(&app, &state, &jar, &ws, workspace_id).await;

    // 3. Owner cookie is not enough without a valid GitHub manifest code.
    let github_state =
        issue_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "github").expect("github state");
    let github_claims = verify_oauth_state(TEST_JWT_SECRET, &github_state)
        .expect("github issue_oauth_state verifies");
    assert_eq!(github_claims.provider, "github");
    let with_cookie = call(
        &app,
        "GET",
        &github_manifest_callback_uri(Some("fake-code"), Some(&github_state)),
        Some(&jar),
        None,
    )
    .await;
    assert_github_manifest_callback_closed(
        &with_cookie,
        "owner GET manifest/callback (valid github state + fake-code)",
    );

    assert_github_not_stored_oauth_app(&app, &state, &jar, &ws, workspace_id).await;

    // 4. Control: /health and GitHub oauth/start still work (not every GET 404).
    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every GET 404s: {}",
        health.body
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
        "A GET /integrations/github/oauth/start must still 200 HTML: {} {}",
        gh_start.status,
        gh_start.raw
    );
    assert!(
        gh_start.raw.contains("github.com/settings/apps/new"),
        "control start must still serve the App Manifest form: {}",
        gh_start.raw
    );
}

async fn assert_github_not_stored_oauth_app(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
    workspace_id: Uuid,
) {
    use fleet_db::repos::integrations as integ_repo;

    let list = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "GET /integrations: {}",
        list.body
    );
    let github = catalog_row(&list.body, "github");
    assert_eq!(
        github["oauth_configured"], false,
        "github must not be configured as a stored oauth-app: {github}"
    );
    assert_eq!(
        github["connected"], false,
        "github must stay disconnected: {github}"
    );
    let catalog_s = format!("{} {}", list.body, github);
    assert!(
        !catalog_s.contains("client_secret"),
        "integrations catalog must not include client_secret: {}",
        list.body
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github creds")
        .is_none(),
        "credentials_for must not have a github row"
    );
    let apps = integ_repo::list_oauth_apps(&state.db, workspace_id)
        .await
        .expect("list_oauth_apps");
    assert!(
        apps.iter().all(|a| a.provider != "github"),
        "oauth_apps must not have a github row for this workspace: {apps:?}"
    );
    assert!(
        apps.is_empty(),
        "this test's workspace must not gain any oauth_apps row: {apps:?}"
    );
}

/// Unsigned / junk-signed GitHub webhook: no 5xx, no secret echo.
/// 204 (today's no-op stub) / 401 / 404 / 400 are the closed statuses.
fn assert_github_webhook_unsigned_closed(out: &HttpOut, label: &str) {
    assert!(
        !out.status.is_server_error(),
        "{label} must not 5xx: {} {}",
        out.status,
        out.raw
    );
    assert!(
        matches!(
            out.status,
            StatusCode::NO_CONTENT
                | StatusCode::UNAUTHORIZED
                | StatusCode::NOT_FOUND
                | StatusCode::BAD_REQUEST
        ),
        "{label} must be 204/401/404/400, got {} {}",
        out.status,
        out.raw
    );
    let hay = format!("{} {}", out.raw, out.body);
    for needle in [
        "client_secret",
        "webhook_secret",
        "pem",
        "access_token",
        "refresh_token",
    ] {
        assert!(
            !hay.contains(needle),
            "{label} must not leak {needle}: {}",
            out.raw
        );
    }
}

async fn assert_github_webhook_not_persisted(
    app: &axum::Router,
    state: &AppState,
    jar: &CookieJar,
    ws: &str,
    workspace_id: Uuid,
    user_id: Uuid,
) {
    use fleet_db::repos::integrations as integ_repo;
    use fleet_db::repos::workspaces as workspace_repo;

    let list = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "GET /integrations: {}",
        list.body
    );
    let github = catalog_row(&list.body, "github");
    assert_eq!(
        github["connected"], false,
        "github must stay disconnected: {github}"
    );
    let catalog_s = format!("{} {}", list.body, github);
    for needle in [
        "client_secret",
        "webhook_secret",
        "pem",
        "access_token",
        "refresh_token",
    ] {
        assert!(
            !catalog_s.contains(needle),
            "integrations catalog must not include {needle}: {}",
            list.body
        );
    }
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github creds")
        .is_none(),
        "credentials_for must not have a github row"
    );
    let conns = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .expect("list connections");
    assert!(
        conns.iter().all(|c| c.provider != "github"),
        "connections table must not have a github row for this workspace: {conns:?}"
    );

    let owned = workspace_repo::list_for_user(&state.db, user_id)
        .await
        .expect("list workspaces after webhook");
    for w in &owned {
        let w_conns = integ_repo::list_all_for_workspace(&state.db, w.id)
            .await
            .expect("list connections for owned workspace");
        assert!(
            w_conns.iter().all(|c| c.provider != "github"),
            "connections table must not have a github row for workspace {} created by this test: {w_conns:?}",
            w.id
        );
        assert!(
            credentials_for(
                &state.db,
                w.id,
                "github",
                &state.integrations.credentials_key
            )
            .await
            .expect("load github creds for owned workspace")
            .is_none(),
            "credentials_for must not have a github row for workspace {}",
            w.id
        );
    }
}

fn catalog_row<'a>(body: &'a Value, provider: &str) -> &'a Value {
    body["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == provider)
        .unwrap_or_else(|| panic!("{provider} missing from catalog: {body}"))
}

fn callback_reports_ok_false(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("ok: false") || lower.contains("ok:false")
}

fn callback_reports_provider_mismatch(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("provider mismatch")
        || (lower.contains("mismatch") && lower.contains("provider"))
        || lower.contains("does not match")
}

/// `verify_oauth_state` only checks signature / typ / exp / nonce. The
/// callback caller must reject a state JWT issued for a different provider
/// *before* consume / exchange / upsert. Trello treats `code` as the token
/// (no live provider), so a skipped check would encrypt the attacker token
/// onto Trello.
#[tokio::test]
async fn oauth_callback_rejects_state_issued_for_different_provider() {
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

    // Both apps configured so a skipped caller check cannot hide behind
    // "{provider} OAuth app is not set" — Trello would store the token,
    // GitHub would consume the nonce then fail exchange.
    for provider in ["trello", "github"] {
        let saved = call(
            &app,
            "PUT",
            &format!("/api/workspaces/{ws}/integrations/{provider}/oauth-app"),
            Some(&jar),
            Some(json!({
                "client_id": format!("{provider}-test-key"),
                "client_secret": format!("{provider}-test-secret"),
            })),
        )
        .await;
        assert_eq!(
            saved.status,
            StatusCode::OK,
            "put {provider} oauth-app: {}",
            saved.body
        );
    }

    let github_state =
        issue_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "github").expect("github state");
    // verify_oauth_state must still accept this JWT — provider matching is
    // the callback's job, not verify's.
    let github_claims =
        verify_oauth_state(TEST_JWT_SECRET, &github_state).expect("verify does not check provider");
    assert_eq!(github_claims.provider, "github");
    assert_ne!(github_claims.provider, "trello");

    let attacker = format!("tok_must_not_land_on_trello_{}", Uuid::now_v7());
    let crossed = call(
        &app,
        "GET",
        &format!("/api/integrations/oauth/callback/trello?code={attacker}&state={github_state}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        crossed.status,
        StatusCode::BAD_REQUEST,
        "provider mismatch must be 400, not 200 popup-as-success: {} {}",
        crossed.status,
        crossed.raw
    );
    assert!(
        callback_reports_ok_false(&crossed.raw),
        "github state on trello callback must fail closed (ok:false), not Connect: {}",
        crossed.raw
    );
    assert!(
        callback_reports_provider_mismatch(&crossed.raw),
        "reject must name provider mismatch, not a later exchange/app error: {}",
        crossed.raw
    );
    assert!(
        !crossed.raw.contains("access_token") && !crossed.raw.contains(&attacker),
        "mismatch must not leak the attacker token: {}",
        crossed.raw
    );

    let after_cross = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(after_cross.status, StatusCode::OK, "{}", after_cross.body);
    assert_eq!(
        catalog_row(&after_cross.body, "trello")["connected"],
        false,
        "github state must not Connect trello: {}",
        after_cross.body
    );
    assert_eq!(
        catalog_row(&after_cross.body, "github")["connected"],
        false,
        "github state completed on trello must not Connect github either: {}",
        after_cross.body
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "trello",
            &state.integrations.credentials_key
        )
        .await
        .expect("load trello creds")
        .is_none(),
        "must not encrypt the attacker token onto trello"
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github creds")
        .is_none(),
        "must not encrypt tokens onto github from a trello callback"
    );

    // Reverse: trello state on github callback. GitHub app is configured, so
    // a skipped check would consume the nonce then fail the live exchange.
    // Caller must reject first so the same JWT can still Connect trello.
    let trello_state =
        issue_oauth_state(TEST_JWT_SECRET, user_id, workspace_id, "trello").expect("trello state");
    let trello_claims =
        verify_oauth_state(TEST_JWT_SECRET, &trello_state).expect("verify still accepts trello");
    assert_eq!(trello_claims.provider, "trello");

    let reverse = call(
        &app,
        "GET",
        &format!(
            "/api/integrations/oauth/callback/github?code=fake-github-code&state={trello_state}"
        ),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        reverse.status,
        StatusCode::BAD_REQUEST,
        "trello state on github callback must be 400, not 200 popup-as-success: {} {}",
        reverse.status,
        reverse.raw
    );
    assert!(
        callback_reports_ok_false(&reverse.raw),
        "trello state on github callback must fail closed: {}",
        reverse.raw
    );
    assert!(
        callback_reports_provider_mismatch(&reverse.raw),
        "reverse reject must name provider mismatch (not consume-then-exchange): {}",
        reverse.raw
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "trello",
            &state.integrations.credentials_key
        )
        .await
        .expect("load trello after reverse")
        .is_none(),
        "github callback must not upsert trello"
    );
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github after reverse")
        .is_none(),
        "trello state must not Connect github"
    );

    // Same trello JWT on the trello callback must still complete — the
    // mismatch path must not consume the nonce.
    let ok_tok = format!("tok_trello_ok_{}", Uuid::now_v7());
    let matched = call(
        &app,
        "GET",
        &format!("/api/integrations/oauth/callback/trello?code={ok_tok}&state={trello_state}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        matched.status,
        StatusCode::OK,
        "matching trello state must complete Connect after a github mismatch reject: {} {}",
        matched.status,
        matched.raw
    );
    let matched_lower = matched.raw.to_ascii_lowercase();
    assert!(
        matched_lower.contains("ok: true") || matched_lower.contains("ok:true"),
        "matching provider must Connect: {}",
        matched.raw
    );
    assert!(
        !callback_reports_ok_false(&matched.raw),
        "matching trello callback must not report ok:false: {}",
        matched.raw
    );

    let stored = credentials_for(
        &state.db,
        workspace_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("load trello after match")
    .expect("matching trello callback must encrypt the token");
    assert_eq!(stored["access_token"], ok_tok);
    assert!(
        credentials_for(
            &state.db,
            workspace_id,
            "github",
            &state.integrations.credentials_key
        )
        .await
        .expect("load github after match")
        .is_none(),
        "successful trello Connect must not write github credentials"
    );

    let after_ok = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(after_ok.status, StatusCode::OK, "{}", after_ok.body);
    assert_eq!(
        catalog_row(&after_ok.body, "trello")["connected"],
        true,
        "matching trello callback must show connected: {}",
        after_ok.body
    );
    assert_eq!(
        catalog_row(&after_ok.body, "github")["connected"],
        false,
        "trello Connect must not mark github connected: {}",
        after_ok.body
    );
}

/// PATCH /integrations/{provider} writes agent defaults only on a connected
/// row. Disconnected / never-connected / unknown / coming_soon must 404 and
/// must not INSERT. Other-ws cookie is 403 and must not change B. Response
/// is IntegrationConnectionPublic (no credentials / tokens).
#[tokio::test]
async fn integration_defaults_patch_only_on_connected_never_invents_connection() {
    use fleet_db::repos::integrations::{self as integ_repo, UpsertConnection};

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("a workspace uuid");
    let b_ws_id = Uuid::parse_str(&b_ws).expect("b workspace uuid");

    let a_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(a_me.status, StatusCode::OK, "A me: {}", a_me.body);
    let a_user = Uuid::parse_str(
        a_me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("A user.id: {}", a_me.body)),
    )
    .expect("A user id");
    let b_me = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(b_me.status, StatusCode::OK, "B me: {}", b_me.body);
    let b_user = Uuid::parse_str(
        b_me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("B user.id: {}", b_me.body)),
    )
    .expect("B user id");

    let tag = Uuid::new_v4();
    let a_access = format!("tok_a_access_{tag}");
    let a_refresh = format!("tok_a_refresh_{tag}");
    let a_secret = format!("client_secret_a_{tag}");
    let b_access = format!("tok_b_access_{tag}");
    let b_refresh = format!("tok_b_refresh_{tag}");
    let b_secret = format!("client_secret_b_{tag}");
    let a_runner = format!("runner-a-{tag}");
    let a_model = format!("model-a-{tag}");
    let steal_runner = format!("runner-steal-{tag}");
    let steal_model = format!("model-steal-{tag}");
    let secrets = [
        a_access.as_str(),
        a_refresh.as_str(),
        a_secret.as_str(),
        b_access.as_str(),
        b_refresh.as_str(),
        b_secret.as_str(),
    ];

    let a_creds = state
        .integrations
        .credentials_key
        .encrypt(&json!({
            "access_token": a_access,
            "refresh_token": a_refresh,
            "client_secret": a_secret,
        }))
        .expect("seal A");
    let b_creds = state
        .integrations
        .credentials_key
        .encrypt(&json!({
            "access_token": b_access,
            "refresh_token": b_refresh,
            "client_secret": b_secret,
        }))
        .expect("seal B");

    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: a_ws_id,
            provider: "github".into(),
            account_label: Some("a-acct".into()),
            config: json!({ "site": "github.com" }),
            credentials: a_creds.clone(),
            connected_by: Some(a_user),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("upsert A github");
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: b_ws_id,
            provider: "github".into(),
            account_label: Some("b-acct".into()),
            config: json!({
                "site": "github.com",
                "default_runner_id": "b-runner",
                "default_provider": "ollama",
                "default_model": "b-model",
            }),
            credentials: b_creds.clone(),
            connected_by: Some(b_user),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("upsert B github");

    let a_before = integ_repo::find_connected(&state.db, a_ws_id, "github")
        .await
        .expect("A github before")
        .expect("A github connected");
    let b_before = integ_repo::find_connected(&state.db, b_ws_id, "github")
        .await
        .expect("B github before")
        .expect("B github connected");

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/github", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "default_runner_id": a_runner,
            "default_provider": "grok",
            "default_model": a_model,
        })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH connected github: {}",
        patched.body
    );
    let obj = patched
        .body
        .as_object()
        .unwrap_or_else(|| panic!("PATCH body object: {}", patched.body));
    assert!(
        !obj.contains_key("credentials"),
        "IntegrationConnectionPublic must not include credentials: {}",
        patched.body
    );
    assert_eq!(patched.body["provider"], "github");
    assert_eq!(patched.body["status"], "connected");
    assert_eq!(patched.body["connected"], true);
    assert_eq!(patched.body["defaults"]["default_runner_id"], a_runner);
    assert_eq!(patched.body["defaults"]["default_provider"], "grok");
    assert_eq!(patched.body["defaults"]["default_model"], a_model);
    assert_eq!(patched.body["config"]["default_runner_id"], a_runner);
    assert_eq!(patched.body["config"]["default_provider"], "grok");
    assert_eq!(patched.body["config"]["default_model"], a_model);
    assert_eq!(patched.body["config"]["site"], "github.com");
    let patched_hay = format!("{} {}", patched.raw, patched.body);
    for needle in ["access_token", "refresh_token", "client_secret"] {
        assert!(
            !patched_hay.contains(needle),
            "PATCH body must not contain {needle}: {}",
            patched.raw
        );
    }
    for secret in secrets {
        assert!(
            !patched_hay.contains(secret),
            "PATCH body must not echo secret: {}",
            patched.raw
        );
    }

    let a_after = integ_repo::find_connected(&state.db, a_ws_id, "github")
        .await
        .expect("A github after patch")
        .expect("A still connected");
    assert_eq!(a_after.credentials, a_before.credentials);
    assert_eq!(a_after.config["default_runner_id"], a_runner);
    assert_eq!(a_after.config["default_provider"], "grok");
    assert_eq!(a_after.config["default_model"], a_model);
    let stored_a = credentials_for(
        &state.db,
        a_ws_id,
        "github",
        &state.integrations.credentials_key,
    )
    .await
    .expect("decrypt A")
    .expect("A creds remain");
    assert_eq!(stored_a["access_token"], a_access);
    assert_eq!(stored_a["refresh_token"], a_refresh);
    assert_eq!(stored_a["client_secret"], a_secret);

    let catalog_ok = call(&app, "GET", &integrations_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        catalog_ok.status,
        StatusCode::OK,
        "GET catalog after PATCH: {}",
        catalog_ok.body
    );
    let gh_view = catalog_row(&catalog_ok.body, "github");
    assert_eq!(gh_view["connected"], true, "{gh_view}");
    assert_eq!(
        gh_view["connection"]["defaults"]["default_runner_id"],
        a_runner
    );
    let catalog_hay = format!("{} {}", catalog_ok.raw, catalog_ok.body);
    for needle in ["access_token", "refresh_token", "client_secret"] {
        assert!(
            !catalog_hay.contains(needle),
            "catalog must not include {needle}: {}",
            catalog_ok.body
        );
    }

    let stolen = call(
        &app,
        "PATCH",
        &format!("{}/github", integrations_path(&a_ws)),
        Some(&b_jar),
        Some(json!({
            "default_runner_id": steal_runner,
            "default_provider": "ollama",
            "default_model": steal_model,
        })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A PATCH must be 403: {}",
        stolen.body
    );
    let stolen_err = stolen.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        stolen_err.contains("not a member"),
        "other-ws 403 must say not a member: {}",
        stolen.body
    );
    let stolen_hay = format!("{} {}", stolen.raw, stolen.body);
    for secret in secrets {
        assert!(
            !stolen_hay.contains(secret),
            "other-ws 403 must not echo secrets: {}",
            stolen.raw
        );
    }

    let a_after_steal = integ_repo::find_connected(&state.db, a_ws_id, "github")
        .await
        .expect("A after other-ws")
        .expect("A still connected");
    assert_eq!(a_after_steal.config["default_runner_id"], a_runner);
    assert_eq!(a_after_steal.config["default_provider"], "grok");
    assert_eq!(a_after_steal.config["default_model"], a_model);
    assert_eq!(a_after_steal.credentials, a_before.credentials);

    let b_after_steal = integ_repo::find_connected(&state.db, b_ws_id, "github")
        .await
        .expect("B after other-ws")
        .expect("B still connected");
    assert_eq!(b_after_steal.config, b_before.config);
    assert_eq!(b_after_steal.credentials, b_before.credentials);
    assert_eq!(b_after_steal.status, "connected");
    let stored_b = credentials_for(
        &state.db,
        b_ws_id,
        "github",
        &state.integrations.credentials_key,
    )
    .await
    .expect("decrypt B")
    .expect("B creds remain");
    assert_eq!(stored_b["access_token"], b_access);

    async fn assert_patch_404_no_insert(
        app: &axum::Router,
        state: &AppState,
        jar: &CookieJar,
        ws: &str,
        ws_id: Uuid,
        provider: &str,
        label: &str,
    ) {
        let before = integ_repo::list_all_for_workspace(&state.db, ws_id)
            .await
            .expect("list before");
        let before_n = before.iter().filter(|c| c.provider == provider).count();
        let out = call(
            app,
            "PATCH",
            &format!("{}/{provider}", integrations_path(ws)),
            Some(jar),
            Some(json!({
                "default_runner_id": "must-not-insert",
                "default_provider": "grok",
                "default_model": "must-not-insert",
            })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "{label} must 404: {} {}",
            out.status,
            out.body
        );
        assert!(
            !out.status.is_success(),
            "{label} must not invent a connection: {} {}",
            out.status,
            out.body
        );
        let after = integ_repo::list_all_for_workspace(&state.db, ws_id)
            .await
            .expect("list after");
        let after_n = after.iter().filter(|c| c.provider == provider).count();
        assert_eq!(
            after_n, before_n,
            "{label} must not INSERT {provider}: before={before:?} after={after:?}"
        );
        assert!(
            after
                .iter()
                .all(|c| c.provider != provider || c.status != "connected"),
            "{label} must not invent a connected {provider} row: {after:?}"
        );
        assert!(
            integ_repo::find_connected(&state.db, ws_id, provider)
                .await
                .expect("find_connected")
                .is_none(),
            "{label} find_connected({provider}) must stay empty"
        );
    }

    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "gitlab",
        "never-connected gitlab",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "notaprovider",
        "unknown provider",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "bitbucket",
        "coming_soon bitbucket",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "confluence",
        "coming_soon confluence",
    )
    .await;

    let b_gitlab = integ_repo::list_all_for_workspace(&state.db, b_ws_id)
        .await
        .expect("B list after 404s");
    assert!(
        b_gitlab.iter().all(|c| c.provider == "github"),
        "failed PATCH on A must not INSERT onto B: {b_gitlab:?}"
    );

    let dropped = integ_repo::disconnect(&state.db, a_ws_id, "github")
        .await
        .expect("disconnect A");
    assert!(dropped, "A github was connected");
    let disc_rows = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A after disconnect");
    let disc = disc_rows
        .iter()
        .find(|c| c.provider == "github")
        .expect("disconnected row remains");
    assert_eq!(disc.status, "disconnected");
    let disc_version = disc.version;
    let disc_creds = disc.credentials.clone();

    let disc_patch = call(
        &app,
        "PATCH",
        &format!("{}/github", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "default_runner_id": steal_runner,
            "default_provider": "grok",
            "default_model": steal_model,
        })),
    )
    .await;
    assert_eq!(
        disc_patch.status,
        StatusCode::NOT_FOUND,
        "disconnected github PATCH must 404: {}",
        disc_patch.body
    );
    let after_disc = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A after disconnected PATCH");
    let github_rows: Vec<_> = after_disc
        .iter()
        .filter(|c| c.provider == "github")
        .collect();
    assert_eq!(
        github_rows.len(),
        1,
        "disconnected PATCH must not INSERT another github row: {after_disc:?}"
    );
    assert_eq!(github_rows[0].status, "disconnected");
    assert_eq!(github_rows[0].version, disc_version);
    assert_eq!(github_rows[0].credentials, disc_creds);
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "github")
            .await
            .expect("find A after disc PATCH")
            .is_none(),
        "disconnected PATCH must not reconnect github"
    );

    let b_still = integ_repo::find_connected(&state.db, b_ws_id, "github")
        .await
        .expect("B after A disconnect")
        .expect("B stays connected");
    assert_eq!(b_still.config, b_before.config);
    assert_eq!(b_still.credentials, b_before.credentials);
}

/// Real Trello Connect (oauth-app + callback), then PATCH agent defaults on
/// that connected row only. Not-connected / unknown / coming_soon must 404
/// and must not INSERT. Other-ws cookie is 403. Public JSON has no secrets.
/// Distinct from planted-github `integration_defaults_patch_only_on_connected_never_invents_connection`.
#[tokio::test]
async fn integration_defaults_after_trello_connect_persist_on_that_row_only() {
    use fleet_db::repos::integrations as integ_repo;

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("a workspace uuid");
    let b_ws_id = Uuid::parse_str(&b_ws).expect("b workspace uuid");

    let a_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(a_me.status, StatusCode::OK, "A me: {}", a_me.body);
    let a_user = Uuid::parse_str(
        a_me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("A user.id: {}", a_me.body)),
    )
    .expect("A user id");

    let tag = Uuid::now_v7();
    let access = format!("tok_trello_defaults_{tag}");
    let client_secret = format!("trello_client_secret_{tag}");
    let client_id = format!("trello-defaults-{tag}");
    let a_runner = format!("runner-trello-{tag}");
    let a_model = format!("model-trello-{tag}");
    let steal_runner = format!("runner-steal-trello-{tag}");
    let steal_model = format!("model-steal-trello-{tag}");
    let secrets = [access.as_str(), client_secret.as_str()];

    let saved = call(
        &app,
        "PUT",
        &format!("{}/trello/oauth-app", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "client_id": client_id,
            "client_secret": client_secret,
        })),
    )
    .await;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "put trello oauth-app: {}",
        saved.body
    );
    let saved_hay = format!("{} {}", saved.raw, saved.body);
    assert!(
        !saved_hay.contains(&client_secret),
        "oauth-app PUT must not echo client_secret: {}",
        saved.raw
    );

    let before_connect = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A list before Connect");
    assert!(
        before_connect.iter().all(|c| c.provider != "trello"),
        "trello must not exist before Connect: {before_connect:?}"
    );

    connect_trello_http(&app, &a_jar, a_user, a_ws_id, &access).await;

    let connected = integ_repo::find_connected(&state.db, a_ws_id, "trello")
        .await
        .expect("find trello after Connect")
        .expect("real Trello Connect must create a connected row");
    assert_eq!(connected.status, "connected");
    assert_eq!(connected.provider, "trello");
    let trello_id = connected.id;
    let creds_before = connected.credentials.clone();
    let stored = credentials_for(
        &state.db,
        a_ws_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("decrypt trello")
    .expect("connected trello stores credentials");
    assert_eq!(stored["access_token"], access);

    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "github")
            .await
            .expect("github after trello Connect")
            .is_none(),
        "trello Connect must not flip github connected"
    );
    assert!(
        integ_repo::find_connected(&state.db, b_ws_id, "trello")
            .await
            .expect("B trello after A Connect")
            .is_none(),
        "A's Trello Connect must not land on B"
    );

    let patched = call(
        &app,
        "PATCH",
        &format!("{}/trello", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "default_runner_id": a_runner,
            "default_provider": "grok",
            "default_model": a_model,
        })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH connected trello: {}",
        patched.body
    );
    let obj = patched
        .body
        .as_object()
        .unwrap_or_else(|| panic!("PATCH body object: {}", patched.body));
    assert!(
        !obj.contains_key("credentials"),
        "IntegrationConnectionPublic must not include credentials: {}",
        patched.body
    );
    assert_eq!(patched.body["provider"], "trello");
    assert_eq!(patched.body["status"], "connected");
    assert_eq!(patched.body["connected"], true);
    assert_eq!(patched.body["defaults"]["default_runner_id"], a_runner);
    assert_eq!(patched.body["defaults"]["default_provider"], "grok");
    assert_eq!(patched.body["defaults"]["default_model"], a_model);
    assert_eq!(patched.body["config"]["default_runner_id"], a_runner);
    assert_eq!(patched.body["config"]["default_provider"], "grok");
    assert_eq!(patched.body["config"]["default_model"], a_model);
    assert_eq!(patched.body["config"]["provider"], "trello");
    let patched_hay = format!("{} {}", patched.raw, patched.body);
    for needle in ["access_token", "refresh_token", "client_secret"] {
        assert!(
            !patched_hay.contains(needle),
            "PATCH body must not contain {needle}: {}",
            patched.raw
        );
    }
    for secret in secrets {
        assert!(
            !patched_hay.contains(secret),
            "PATCH body must not echo secret: {}",
            patched.raw
        );
    }

    let a_after = integ_repo::find_connected(&state.db, a_ws_id, "trello")
        .await
        .expect("A trello after PATCH")
        .expect("trello still connected");
    assert_eq!(a_after.id, trello_id, "PATCH must update the Connect row");
    assert_eq!(a_after.credentials, creds_before);
    assert_eq!(a_after.config["default_runner_id"], a_runner);
    assert_eq!(a_after.config["default_provider"], "grok");
    assert_eq!(a_after.config["default_model"], a_model);
    let rows_after = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A list after trello PATCH");
    let trello_rows: Vec<_> = rows_after
        .iter()
        .filter(|c| c.provider == "trello")
        .collect();
    assert_eq!(
        trello_rows.len(),
        1,
        "PATCH must not INSERT a second trello row: {rows_after:?}"
    );
    assert!(
        rows_after.iter().all(|c| c.provider == "trello"),
        "trello PATCH must persist defaults on that row only: {rows_after:?}"
    );

    let catalog_ok = call(&app, "GET", &integrations_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        catalog_ok.status,
        StatusCode::OK,
        "GET catalog after trello PATCH: {}",
        catalog_ok.body
    );
    let trello_view = catalog_row(&catalog_ok.body, "trello");
    assert_eq!(trello_view["connected"], true, "{trello_view}");
    assert_eq!(
        trello_view["connection"]["defaults"]["default_runner_id"],
        a_runner
    );
    assert_eq!(
        trello_view["connection"]["defaults"]["default_provider"],
        "grok"
    );
    assert_eq!(
        trello_view["connection"]["defaults"]["default_model"],
        a_model
    );
    assert_eq!(catalog_row(&catalog_ok.body, "github")["connected"], false);
    let catalog_hay = format!("{} {}", catalog_ok.raw, catalog_ok.body);
    for needle in ["access_token", "refresh_token", "client_secret"] {
        assert!(
            !catalog_hay.contains(needle),
            "GET /integrations must not include {needle}: {}",
            catalog_ok.body
        );
    }
    for secret in secrets {
        assert!(
            !catalog_hay.contains(secret),
            "GET /integrations must not echo secret: {}",
            catalog_ok.raw
        );
    }

    let gh_patch = call(
        &app,
        "PATCH",
        &format!("{}/github", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "default_runner_id": "must-not-create-github",
            "default_provider": "grok",
            "default_model": "must-not-create-github",
        })),
    )
    .await;
    assert_eq!(
        gh_patch.status,
        StatusCode::NOT_FOUND,
        "PATCH never-connected github after trello Connect must 404: {}",
        gh_patch.body
    );
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "github")
            .await
            .expect("github after stray PATCH")
            .is_none(),
        "trello defaults PATCH must not invent a github connection"
    );
    let trello_still = integ_repo::find_connected(&state.db, a_ws_id, "trello")
        .await
        .expect("trello after github 404")
        .expect("trello stays connected");
    assert_eq!(trello_still.id, trello_id);
    assert_eq!(trello_still.config["default_runner_id"], a_runner);
    assert_eq!(trello_still.credentials, creds_before);

    let stolen = call(
        &app,
        "PATCH",
        &format!("{}/trello", integrations_path(&a_ws)),
        Some(&b_jar),
        Some(json!({
            "default_runner_id": steal_runner,
            "default_provider": "ollama",
            "default_model": steal_model,
        })),
    )
    .await;
    assert_eq!(
        stolen.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A trello PATCH must be 403: {}",
        stolen.body
    );
    let stolen_err = stolen.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        stolen_err.contains("not a member"),
        "other-ws 403 must say not a member: {}",
        stolen.body
    );
    let stolen_hay = format!("{} {}", stolen.raw, stolen.body);
    for secret in secrets {
        assert!(
            !stolen_hay.contains(secret),
            "other-ws 403 must not echo secrets: {}",
            stolen.raw
        );
    }
    let a_after_steal = integ_repo::find_connected(&state.db, a_ws_id, "trello")
        .await
        .expect("A after other-ws")
        .expect("A trello still connected");
    assert_eq!(a_after_steal.config["default_runner_id"], a_runner);
    assert_eq!(a_after_steal.config["default_provider"], "grok");
    assert_eq!(a_after_steal.config["default_model"], a_model);
    assert_eq!(a_after_steal.credentials, creds_before);
    assert!(
        integ_repo::find_connected(&state.db, b_ws_id, "trello")
            .await
            .expect("B trello after steal")
            .is_none(),
        "other-ws PATCH must not write B's trello"
    );
    let b_rows = integ_repo::list_all_for_workspace(&state.db, b_ws_id)
        .await
        .expect("B list after steal");
    assert!(
        b_rows.is_empty(),
        "other-ws PATCH must not INSERT onto B: {b_rows:?}"
    );

    async fn assert_patch_404_no_insert(
        app: &axum::Router,
        state: &AppState,
        jar: &CookieJar,
        ws: &str,
        ws_id: Uuid,
        provider: &str,
        label: &str,
    ) {
        let before = integ_repo::list_all_for_workspace(&state.db, ws_id)
            .await
            .expect("list before");
        let before_n = before.iter().filter(|c| c.provider == provider).count();
        let out = call(
            app,
            "PATCH",
            &format!("{}/{provider}", integrations_path(ws)),
            Some(jar),
            Some(json!({
                "default_runner_id": "must-not-insert",
                "default_provider": "grok",
                "default_model": "must-not-insert",
            })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "{label} must 404: {} {}",
            out.status,
            out.body
        );
        let after = integ_repo::list_all_for_workspace(&state.db, ws_id)
            .await
            .expect("list after");
        let after_n = after.iter().filter(|c| c.provider == provider).count();
        assert_eq!(
            after_n, before_n,
            "{label} must not INSERT {provider}: before={before:?} after={after:?}"
        );
        assert!(
            after
                .iter()
                .all(|c| c.provider != provider || c.status != "connected"),
            "{label} must not invent a connected {provider} row: {after:?}"
        );
        assert!(
            integ_repo::find_connected(&state.db, ws_id, provider)
                .await
                .expect("find_connected")
                .is_none(),
            "{label} find_connected({provider}) must stay empty"
        );
    }

    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "gitlab",
        "never-connected gitlab",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "notaprovider",
        "unknown provider",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "bitbucket",
        "coming_soon bitbucket",
    )
    .await;
    assert_patch_404_no_insert(
        &app,
        &state,
        &a_jar,
        &a_ws,
        a_ws_id,
        "confluence",
        "coming_soon confluence",
    )
    .await;

    let trello_after_404s = integ_repo::find_connected(&state.db, a_ws_id, "trello")
        .await
        .expect("trello after 404s")
        .expect("failed PATCH must not drop trello");
    assert_eq!(trello_after_404s.id, trello_id);
    assert_eq!(trello_after_404s.config["default_runner_id"], a_runner);

    let dropped = integ_repo::disconnect(&state.db, a_ws_id, "trello")
        .await
        .expect("disconnect A trello");
    assert!(dropped, "A trello was connected");
    let disc_rows = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A after disconnect");
    let disc = disc_rows
        .iter()
        .find(|c| c.provider == "trello")
        .expect("disconnected trello row remains");
    assert_eq!(disc.status, "disconnected");
    let disc_version = disc.version;
    let disc_creds = disc.credentials.clone();

    let disc_patch = call(
        &app,
        "PATCH",
        &format!("{}/trello", integrations_path(&a_ws)),
        Some(&a_jar),
        Some(json!({
            "default_runner_id": steal_runner,
            "default_provider": "grok",
            "default_model": steal_model,
        })),
    )
    .await;
    assert_eq!(
        disc_patch.status,
        StatusCode::NOT_FOUND,
        "disconnected trello PATCH must 404: {}",
        disc_patch.body
    );
    let after_disc = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("A after disconnected PATCH");
    let trello_left: Vec<_> = after_disc
        .iter()
        .filter(|c| c.provider == "trello")
        .collect();
    assert_eq!(
        trello_left.len(),
        1,
        "disconnected PATCH must not INSERT another trello row: {after_disc:?}"
    );
    assert_eq!(trello_left[0].status, "disconnected");
    assert_eq!(trello_left[0].version, disc_version);
    assert_eq!(trello_left[0].credentials, disc_creds);
    assert!(
        integ_repo::find_connected(&state.db, a_ws_id, "trello")
            .await
            .expect("find A after disc PATCH")
            .is_none(),
        "disconnected PATCH must not reconnect trello"
    );
    let b_still = integ_repo::list_all_for_workspace(&state.db, b_ws_id)
        .await
        .expect("B after A disconnect");
    assert!(
        b_still.is_empty(),
        "A's trello disconnect/PATCH must not write B: {b_still:?}"
    );
}

#[tokio::test]
async fn chat_status_create_list_delete_other_user_forbidden() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let chat = format!("/api/workspaces/{ws}/chat");

    let status = call(&app, "GET", &format!("{chat}/status"), Some(&jar), None).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    let providers = status.body["providers"].as_array().expect("providers");
    assert!(
        providers.iter().all(|p| p["id"].as_str() == Some("openai")),
        "without runner_id, only plane OpenAI (not a runner inventory): {}",
        status.body
    );
    let hint = status.body["hint"].as_str().unwrap_or("");
    assert!(
        hint.to_ascii_lowercase().contains("runner"),
        "status must point at the runner inventory: {hint}"
    );

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

/// Docs §6.2: new chat POSTs `/conversations` with title = first 72 chars.
/// `create_conversation` must not persist a title longer than 72 (400, or
/// 201 with stored title/name `.len() <= 72`). Empty / whitespace may 201 as
/// "New chat" — storing `""` is a fail. Distinct from launch-title-max-200,
/// cases-title-1-to-300, and conversation IDOR. Filter
/// `-- conversation_title`. Keep
/// `chat_status_create_list_delete_other_user_forbidden`.
#[tokio::test]
async fn conversation_title_over_72_is_400_or_clamped_and_does_not_persist() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let conversations = chat_conversations_path(&ws);

    let over_73 = "x".repeat(73);
    let over_200 = "x".repeat(200);
    assert_eq!(over_73.len(), 73);
    assert_eq!(over_200.len(), 200);

    for (title, label) in [(&over_73[..], "73-char"), (over_200.as_str(), "200-char")] {
        let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
        assert_eq!(
            snap.status,
            StatusCode::OK,
            "snapshot GET /conversations before {label}: {}",
            snap.body
        );
        assert!(
            !conversation_list_contains_blob(&snap.body, title),
            "pre-call GET /conversations must not already contain the {label} title"
        );

        let out = call(
            &app,
            "POST",
            &conversations,
            Some(&jar),
            Some(json!({ "title": title })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::CREATED,
            "{label} title must 201 with a clamped title, not {}: {}",
            out.status,
            out.body
        );
        assert_conversation_title_fields_at_most_72(&out.body, &format!("{label} 201 body"));
        let created_title = conversation_create_title(&out.body);
        assert!(
            created_title.len() <= 72,
            "{label} 201 title must be clamped to <=72, got len={} {created_title:?}: {}",
            created_title.len(),
            out.body
        );
        assert_ne!(
            created_title.as_str(),
            title,
            "{label} 201 must not persist the unclamped title: {}",
            out.body
        );
        if let Some(name) = out.body["conversation"]["name"].as_str() {
            assert_ne!(
                name, title,
                "{label} 201 name must not be the unclamped title: {}",
                out.body
            );
        }
        let cid = out.body["conversation"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{label} 201 must return conversation.id: {}", out.body))
            .to_string();

        let listed = call(&app, "GET", &conversations, Some(&jar), None).await;
        assert_eq!(
            listed.status,
            StatusCode::OK,
            "GET /conversations after {label} 201: {}",
            listed.body
        );
        let listed_rows = listed_conversation_id_titles(&listed.body);
        let stored = listed_rows
            .iter()
            .find(|(id, _)| id == &cid)
            .map(|(_, title)| title.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "{label} 201 must appear on GET /conversations ({cid}): {}",
                    listed.body
                )
            });
        assert!(
            stored.len() <= 72,
            "{label} GET /conversations title must be clamped to <=72, got len={} {stored:?}: {}",
            stored.len(),
            listed.body
        );
        assert_ne!(
            stored, title,
            "{label} GET /conversations must not persist the unclamped title: {}",
            listed.body
        );
        assert!(
            !listed.raw.contains(title),
            "{label} GET /conversations must not include the unclamped {label} string"
        );

        let got = call(
            &app,
            "GET",
            &chat_conversation_path(&ws, &cid),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            got.status,
            StatusCode::OK,
            "GET /conversations/{{id}} after {label} 201: {}",
            got.body
        );
        assert_conversation_title_fields_at_most_72(
            &got.body,
            &format!("{label} GET /conversations/{{id}}"),
        );
        let got_title = got.body["conversation"]["title"]
            .as_str()
            .unwrap_or_else(|| panic!("{label} GET title: {}", got.body));
        assert_ne!(
            got_title, title,
            "{label} GET /conversations/{{id}} must not persist the unclamped title: {}",
            got.body
        );
    }

    for (title, label) in [("", "empty"), ("   ", "whitespace")] {
        let out = call(
            &app,
            "POST",
            &conversations,
            Some(&jar),
            Some(json!({ "title": title })),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::CREATED,
            "{label} title must 201 with a non-empty title, not {}: {}",
            out.status,
            out.body
        );
        let stored = conversation_create_title(&out.body);
        assert!(
            !stored.is_empty() && !stored.trim().is_empty(),
            "{label} POST must not store an empty title (201 as \"New chat\"): {}",
            out.body
        );
        assert_eq!(
            stored, "New chat",
            "{label} POST empty/whitespace title must become \"New chat\": {}",
            out.body
        );
        let cid = out.body["conversation"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{label} 201 conversation.id: {}", out.body))
            .to_string();
        let listed = call(&app, "GET", &conversations, Some(&jar), None).await;
        assert_eq!(
            listed.status,
            StatusCode::OK,
            "GET /conversations after {label} 201: {}",
            listed.body
        );
        let listed_title = listed_conversation_id_titles(&listed.body)
            .into_iter()
            .find(|(id, _)| id == &cid)
            .map(|(_, t)| t)
            .unwrap_or_else(|| {
                panic!(
                    "{label} 201 must appear on GET /conversations ({cid}): {}",
                    listed.body
                )
            });
        assert!(
            !listed_title.is_empty() && !listed_title.trim().is_empty(),
            "{label} GET /conversations must not list an empty title: {}",
            listed.body
        );
    }

    // Control: 72 x's still 201 and GET shows that title so this is not a
    // tautology that every create 400s.
    let at_cap = "x".repeat(72);
    assert_eq!(at_cap.len(), 72);
    let control = call(
        &app,
        "POST",
        &conversations,
        Some(&jar),
        Some(json!({ "title": at_cap })),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::CREATED,
        "control 72-char title must 201 (not a tautology that every create 400s): {}",
        control.body
    );
    assert_eq!(
        conversation_create_title(&control.body),
        at_cap,
        "control POST must persist the exact 72-char title: {}",
        control.body
    );
    let control_cid = control.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control conversation.id: {}", control.body))
        .to_string();
    let after_control = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after_control.status,
        StatusCode::OK,
        "GET /conversations after 72-char create: {}",
        after_control.body
    );
    assert!(
        listed_conversation_id_titles(&after_control.body)
            .iter()
            .any(|(id, title)| id == &control_cid && title == &at_cap),
        "GET /conversations must show the 72-char title: {}",
        after_control.body
    );
    let got_control = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, &control_cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got_control.status,
        StatusCode::OK,
        "GET /conversations/{{id}} after 72-char create: {}",
        got_control.body
    );
    assert_eq!(
        got_control.body["conversation"]["title"].as_str(),
        Some(at_cap.as_str()),
        "GET /conversations/{{id}} must show the 72-char title: {}",
        got_control.body
    );

    // Optional: POST /chat auto-create from a 100-char first user line
    // (title_from_user_text). Skip if the stub provider cannot complete.
    let first_line = "z".repeat(100);
    assert_eq!(first_line.len(), 100);
    let auto = call(
        &app,
        "POST",
        &chat_complete_path(&ws),
        Some(&jar),
        Some(json!({
            "messages": [{ "role": "user", "content": first_line }],
            "provider": "ollama",
        })),
    )
    .await;
    assert!(
        !auto.status.is_server_error(),
        "optional POST /chat must not 500: {} {}",
        auto.status,
        auto.body
    );
    if auto.status.is_success() {
        let cid = auto.body["conversation_id"]
            .as_str()
            .or_else(|| auto.body["conversation"]["id"].as_str())
            .unwrap_or_else(|| {
                panic!(
                    "optional POST /chat success must return conversation_id: {}",
                    auto.body
                )
            });
        let got = call(
            &app,
            "GET",
            &chat_conversation_path(&ws, cid),
            Some(&jar),
            None,
        )
        .await;
        if got.status == StatusCode::OK {
            assert_conversation_title_fields_at_most_72(
                &got.body,
                "optional POST /chat auto-created conversation",
            );
        }
        let listed = call(&app, "GET", &conversations, Some(&jar), None).await;
        if listed.status == StatusCode::OK {
            if let Some((_, title)) = listed_conversation_id_titles(&listed.body)
                .into_iter()
                .find(|(id, _)| id == cid)
            {
                assert!(
                    title.len() <= 72,
                    "optional POST /chat auto title must be <=72, got len={} {title:?}: {}",
                    title.len(),
                    listed.body
                );
            }
        }
    }
}

fn conversation_create_title(body: &Value) -> String {
    body["conversation"]["title"]
        .as_str()
        .or_else(|| body["title"].as_str())
        .unwrap_or_else(|| panic!("conversation title: {body}"))
        .to_string()
}

fn assert_conversation_title_fields_at_most_72(body: &Value, what: &str) {
    let conv = if body.get("conversation").is_some() {
        &body["conversation"]
    } else {
        body
    };
    for key in ["title", "name"] {
        if let Some(got) = conv[key].as_str() {
            assert!(
                got.len() <= 72,
                "{what} {key} must be <=72 chars, got len={} {got:?}: {body}",
                got.len()
            );
        }
    }
}

/// Docs §6.2/§14: cookie GET `/chat/status` and `/fleet/settings` must never
/// echo plane LLM secrets. Distinct from heartbeat `agents[]` tokens,
/// password hashes, spawn env, and `repo_url` userinfo. Filter
/// `-- chat_status_omits_llm_keys`.
const CHAT_STATUS_SECRET_KEYS: &[&str] = &["api_key", "access_token", "authorization"];

fn json_object_has_key_ci(v: &Value, key: &str) -> bool {
    let want = key.to_ascii_lowercase();
    match v {
        Value::Object(map) => {
            map.keys().any(|k| k.to_ascii_lowercase() == want)
                || map.values().any(|child| json_object_has_key_ci(child, key))
        }
        Value::Array(arr) => arr.iter().any(|child| json_object_has_key_ci(child, key)),
        _ => false,
    }
}

fn collect_base_urls(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(url) = map.get("base_url").and_then(Value::as_str) {
                out.push(url.to_string());
            }
            for child in map.values() {
                collect_base_urls(child, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                collect_base_urls(child, out);
            }
        }
        _ => {}
    }
}

fn assert_raw_omits_llm_secrets(out: &HttpOut, secrets: &[&str], what: &str) {
    for secret in secrets {
        assert!(
            !secret.is_empty() && !out.raw.contains(secret),
            "{what} raw body must not contain {secret}: {}",
            out.raw
        );
    }
}

fn assert_chat_status_omits_llm_keys(out: &HttpOut, secrets: &[&str], what: &str) {
    assert_eq!(out.status, StatusCode::OK, "{what}: {}", out.body);
    assert_raw_omits_llm_secrets(out, secrets, what);
    let providers = out.body["providers"]
        .as_array()
        .unwrap_or_else(|| panic!("{what} must include providers[]: {}", out.body));
    assert!(
        !providers.is_empty(),
        "{what} must still return a providers array (not a tautology that every GET is empty): {}",
        out.body
    );
    for provider in providers {
        for key in CHAT_STATUS_SECRET_KEYS {
            assert!(
                provider.get(*key).is_none()
                    && !provider
                        .as_object()
                        .map(|m| m.keys().any(|k| k.eq_ignore_ascii_case(key)))
                        .unwrap_or(false),
                "{what} providers[] must not have {key}: {provider}"
            );
        }
    }
    assert!(
        !json_object_has_key_ci(&out.body, "api_key")
            && !json_object_has_key_ci(&out.body, "access_token")
            && !json_object_has_key_ci(&out.body, "authorization"),
        "{what} JSON must not carry api_key / access_token / authorization: {}",
        out.body
    );
    let mut urls = Vec::new();
    collect_base_urls(&out.body, &mut urls);
    for url in &urls {
        assert!(
            !https_url_has_userinfo(url),
            "{what} base_url must not include userinfo: {url}"
        );
    }
}

/// In-struct plane keys (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` /
/// `GROK_API_KEY` / Ollama userinfo) must not appear on cookie JSON.
/// `ChatProviderInfo` is id/name/available/models — this drives the oneshot
/// raw body, not an `openai_status` field-list unit. Keep
/// `chat_status_create_list_delete_other_user_forbidden`. Do not weaken
/// list-chat-foreign-runner-id-404 or list-heartbeat-agents-omit-tokens.
#[tokio::test]
async fn chat_status_omits_llm_keys() {
    let (_base_app, mut state) = app_with_state().await;
    let stamp = Uuid::new_v4();
    let openai_key = format!("sk-openai-status-omit-{stamp}");
    let claude_key = format!("sk-claude-status-omit-{stamp}");
    let grok_key = format!("sk-grok-status-omit-{stamp}");
    let ollama_pass = "UNIQUEPASS";
    let ollama_userinfo = "ollama-user:UNIQUEPASS";
    state.openai.api_key = Some(openai_key.clone());
    state.claude.api_key = Some(claude_key.clone());
    state.grok.api_key = Some(grok_key.clone());
    state.ollama.base_url = "http://ollama-user:UNIQUEPASS@127.0.0.1:11434".into();
    assert!(
        state.openai.enabled && state.openai.available(),
        "fixture must set a usable OpenAI key so providers[] is not vacuously empty"
    );
    let app = build_router().with_state(state);

    let (_email, jar, ws) = signup_and_login(&app).await;
    let secrets = [
        openai_key.as_str(),
        claude_key.as_str(),
        grok_key.as_str(),
        ollama_pass,
        ollama_userinfo,
    ];

    let status = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/chat/status"),
        Some(&jar),
        None,
    )
    .await;
    assert_chat_status_omits_llm_keys(&status, &secrets, "GET /chat/status (no runner_id)");
    let providers = status.body["providers"].as_array().expect("providers");
    let openai = providers
        .iter()
        .find(|p| p["id"].as_str() == Some("openai"))
        .unwrap_or_else(|| {
            panic!(
                "providers[] may list openai once a key is set: {}",
                status.body
            )
        });
    assert_eq!(
        openai["available"], true,
        "openai must be available once a key is set: {}",
        status.body
    );

    let hid = create_paired_online_runner(&app, &jar, &ws).await;
    let runner_status = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/chat/status?runner_id={hid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_chat_status_omits_llm_keys(
        &runner_status,
        &secrets,
        &format!("GET /chat/status?runner_id={hid}"),
    );

    let fleet_got = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        fleet_got.status,
        StatusCode::OK,
        "GET /fleet: {}",
        fleet_got.body
    );
    assert_raw_omits_llm_secrets(&fleet_got, &secrets, "GET /fleet");
    let fleet_cfg = &fleet_got.body["config"];
    assert!(
        fleet_cfg["grok_env"].is_boolean(),
        "GET /fleet grok_env may be a boolean: {}",
        fleet_got.body
    );
    for field in ["fleet_llm", "effective_backend"] {
        let got = fleet_cfg[field].as_str().unwrap_or("");
        assert!(
            got == "off" || got == "ollama",
            "GET /fleet {field} may be off|ollama, got {got:?}: {}",
            fleet_got.body
        );
    }

    let settings = call(&app, "GET", &fleet(&ws, "/settings"), Some(&jar), None).await;
    assert_eq!(
        settings.status,
        StatusCode::OK,
        "GET /fleet/settings: {}",
        settings.body
    );
    assert_raw_omits_llm_secrets(&settings, &secrets, "GET /fleet/settings");
    assert!(
        settings.body["grok_env"].is_boolean(),
        "GET /fleet/settings grok_env may be a boolean: {}",
        settings.body
    );
    for field in ["fleet_llm", "effective_backend"] {
        let got = settings.body[field].as_str().unwrap_or("");
        assert!(
            got == "off" || got == "ollama",
            "GET /fleet/settings {field} may be off|ollama, got {got:?}: {}",
            settings.body
        );
    }

    let control = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/chat/status"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "control GET /chat/status: {}",
        control.body
    );
    let control_providers = control.body["providers"]
        .as_array()
        .unwrap_or_else(|| panic!("control providers[]: {}", control.body));
    assert!(
        !control_providers.is_empty(),
        "control GET /chat/status must still return providers so this is not a tautology that every GET is empty: {}",
        control.body
    );
}

/// GROK_ENABLED=false / OLLAMA_ENABLED=false must 400 complete and stream
/// without inserting a conversation. Distinct from openai/claude unset
/// (`enabled=true`, `api_key=None`) and grok.rs missing-key `--lib`.
/// Filter `-- grok_ollama_disabled`.
#[tokio::test]
async fn chat_grok_ollama_disabled_no_upstream() {
    let (app, state) = app_with_state().await;
    assert!(
        !state.grok.enabled && state.grok.api_key.is_none(),
        "app() fixture must keep grok.enabled=false and api_key=None"
    );
    assert!(
        !state.ollama.enabled,
        "app() fixture must keep ollama.enabled=false"
    );

    let (_email, jar, ws) = signup_and_login(&app).await;
    let chat = format!("/api/workspaces/{ws}/chat");
    let conversations = format!("{chat}/conversations");

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);

    let grok_content = format!("unique-grok-disabled-{}", Uuid::new_v4());
    let ollama_content = format!("unique-ollama-disabled-{}", Uuid::new_v4());
    let grok_body = json!({
        "provider": "grok",
        "model": "grok-4",
        "messages": [{ "role": "user", "content": grok_content }],
    });
    let ollama_body = json!({
        "provider": "ollama",
        "model": "llama3.2",
        "messages": [{ "role": "user", "content": ollama_content }],
    });

    let grok = call_disabled_provider_timed(&app, &jar, &chat, grok_body.clone()).await;
    assert_disabled_provider_complete(&grok, "grok", "GROK_ENABLED");
    let ollama = call_disabled_provider_timed(&app, &jar, &chat, ollama_body.clone()).await;
    assert_disabled_provider_complete(&ollama, "ollama", "OLLAMA_ENABLED");

    let after_complete = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(
        &snap,
        &after_complete,
        &[&grok_content, &ollama_content],
        "after disabled POST /chat",
    );
    assert_new_conversation_ids_are_404(
        &app,
        &jar,
        &ws,
        &snap_rows,
        &after_complete,
        &[&grok, &ollama],
        "after disabled POST /chat",
    )
    .await;
    assert_unique_turns_not_persisted(
        &app,
        &jar,
        &ws,
        &after_complete,
        &[&grok_content, &ollama_content],
        "after disabled POST /chat",
    )
    .await;

    let grok_stream =
        call_disabled_provider_timed(&app, &jar, &format!("{chat}/stream"), grok_body.clone())
            .await;
    assert_disabled_provider_stream(&grok_stream, "grok", "GROK_ENABLED");
    let ollama_stream =
        call_disabled_provider_timed(&app, &jar, &format!("{chat}/stream"), ollama_body.clone())
            .await;
    assert_disabled_provider_stream(&ollama_stream, "ollama", "OLLAMA_ENABLED");

    let after_stream = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(
        &snap,
        &after_stream,
        &[&grok_content, &ollama_content],
        "after disabled POST /chat/stream",
    );
    assert_new_conversation_ids_are_404(
        &app,
        &jar,
        &ws,
        &snap_rows,
        &after_stream,
        &[&grok_stream, &ollama_stream],
        "after disabled POST /chat/stream",
    )
    .await;
    assert_unique_turns_not_persisted(
        &app,
        &jar,
        &ws,
        &after_stream,
        &[&grok_content, &ollama_content],
        "after disabled POST /chat/stream",
    )
    .await;

    let unique_key = format!("sk-grok-disabled-lock-{}", Uuid::new_v4());
    let mut keyed_state = state.clone();
    keyed_state.grok.enabled = false;
    keyed_state.grok.api_key = Some(unique_key.clone());
    keyed_state.ollama.enabled = false;
    keyed_state.ollama.base_url = "http://127.0.0.1:9".into();
    let keyed_app = build_router().with_state(keyed_state);
    let (_k_email, k_jar, k_ws) = signup_and_login(&keyed_app).await;
    let k_chat = format!("/api/workspaces/{k_ws}/chat");
    let k_grok_content = format!("unique-keyed-grok-disabled-{}", Uuid::new_v4());
    let k_ollama_content = format!("unique-keyed-ollama-disabled-{}", Uuid::new_v4());
    let k_grok = call_disabled_provider_timed(
        &keyed_app,
        &k_jar,
        &k_chat,
        json!({
            "provider": "grok",
            "model": "grok-4",
            "messages": [{ "role": "user", "content": k_grok_content }],
        }),
    )
    .await;
    assert_disabled_provider_complete(&k_grok, "grok", "GROK_ENABLED");
    assert_disabled_body_omits_secret(&k_grok, &unique_key, "keyed grok POST /chat");
    let k_ollama = call_disabled_provider_timed(
        &keyed_app,
        &k_jar,
        &k_chat,
        json!({
            "provider": "ollama",
            "model": "llama3.2",
            "messages": [{ "role": "user", "content": k_ollama_content }],
        }),
    )
    .await;
    assert_disabled_provider_complete(&k_ollama, "ollama", "OLLAMA_ENABLED");
    assert_disabled_body_omits_secret(&k_ollama, &unique_key, "keyed ollama POST /chat");

    let status = call(&app, "GET", &format!("{chat}/status"), Some(&jar), None).await;
    assert_eq!(
        status.status,
        StatusCode::OK,
        "control GET /chat/status: {}",
        status.body
    );
    let control_title = format!("unique-control-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &conversations,
        Some(&jar),
        Some(json!({ "title": control_title })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "control POST /conversations must still 201 so this is not a tautology that every chat POST 400s: {}",
        created.body
    );
    assert_eq!(
        created.body["conversation"]["title"].as_str(),
        Some(control_title.as_str()),
        "control title: {}",
        created.body
    );
}

/// POST /chat openai|claude with the app() fixture (`enabled=true`,
/// `api_key=None`) must 400 and must not INSERT a conversation or open
/// api.openai.com / api.anthropic.com. Distinct from
/// `chat_grok_ollama_disabled_no_upstream` (plane GROK_ENABLED /
/// OLLAMA_ENABLED=false), grok.rs `--lib` missing-key, and
/// `list-chat-status-omits-llm-keys` (GET /status when keys ARE set).
/// Filter `-- openai_claude_unset`.
#[tokio::test]
async fn chat_openai_claude_unset_no_upstream() {
    let (app, state) = app_with_state().await;
    assert!(
        state.openai.enabled && state.openai.api_key.is_none(),
        "app() fixture must keep openai.enabled=true and api_key=None (do not flip to a real key)"
    );
    assert!(
        state.claude.enabled && state.claude.api_key.is_none(),
        "app() fixture must keep claude.enabled=true and api_key=None (do not flip to a real key)"
    );
    assert!(
        !state.openai.available() && !state.claude.available(),
        "unset keys must not make available() true"
    );

    let env_key_values = process_llm_key_values();
    let (_email, jar, ws) = signup_and_login(&app).await;
    let chat = format!("/api/workspaces/{ws}/chat");
    let conversations = format!("{chat}/conversations");

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);

    let openai_content = format!("unique-openai-unset-{}", Uuid::new_v4());
    let claude_content = format!("unique-claude-unset-{}", Uuid::new_v4());
    let openai_body = json!({
        "provider": "openai",
        "model": "gpt-4o-mini",
        "messages": [{ "role": "user", "content": openai_content }],
    });
    let claude_body = json!({
        "provider": "claude",
        "model": "claude-sonnet-4-5",
        "messages": [{ "role": "user", "content": claude_content }],
    });

    let openai = call_unset_provider_timed(&app, &jar, &chat, openai_body.clone()).await;
    assert_unset_provider_complete(&openai, "openai", &env_key_values);
    let claude = call_unset_provider_timed(&app, &jar, &chat, claude_body.clone()).await;
    assert_unset_provider_complete(&claude, "claude", &env_key_values);

    let after_complete = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(
        &snap,
        &after_complete,
        &[&openai_content, &claude_content],
        "after unset POST /chat",
    );
    assert_new_conversation_ids_are_404(
        &app,
        &jar,
        &ws,
        &snap_rows,
        &after_complete,
        &[&openai, &claude],
        "after unset POST /chat",
    )
    .await;
    assert_unique_turns_not_persisted(
        &app,
        &jar,
        &ws,
        &after_complete,
        &[&openai_content, &claude_content],
        "after unset POST /chat",
    )
    .await;

    let openai_stream =
        call_unset_provider_timed(&app, &jar, &format!("{chat}/stream"), openai_body.clone()).await;
    assert_unset_provider_stream(&openai_stream, "openai", &env_key_values);
    let claude_stream =
        call_unset_provider_timed(&app, &jar, &format!("{chat}/stream"), claude_body.clone()).await;
    assert_unset_provider_stream(&claude_stream, "claude", &env_key_values);

    let after_stream = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(
        &snap,
        &after_stream,
        &[&openai_content, &claude_content],
        "after unset POST /chat/stream",
    );
    assert_new_conversation_ids_are_404(
        &app,
        &jar,
        &ws,
        &snap_rows,
        &after_stream,
        &[&openai_stream, &claude_stream],
        "after unset POST /chat/stream",
    )
    .await;
    assert_unique_turns_not_persisted(
        &app,
        &jar,
        &ws,
        &after_stream,
        &[&openai_content, &claude_content],
        "after unset POST /chat/stream",
    )
    .await;

    let status = call(&app, "GET", &format!("{chat}/status"), Some(&jar), None).await;
    assert_eq!(
        status.status,
        StatusCode::OK,
        "control GET /chat/status: {}",
        status.body
    );
    assert_unset_status_providers_unavailable(&status);
    assert_unset_no_llm_secrets(&status, &env_key_values, "control GET /chat/status");

    let control_title = format!("unique-openai-claude-unset-control-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &conversations,
        Some(&jar),
        Some(json!({ "title": control_title })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "control POST /conversations must still 201 so this is not a tautology that every chat POST 400s: {}",
        created.body
    );
    assert_eq!(
        created.body["conversation"]["title"].as_str(),
        Some(control_title.as_str()),
        "control title: {}",
        created.body
    );
}

async fn call_disabled_provider_timed(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    body: Value,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, "POST", path, Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "POST {path} hung >5s — disabled grok/ollama must not open api.x.ai or 127.0.0.1:11434"
        )
    })
}

fn assert_disabled_no_llm_secrets(out: &HttpOut, label: &str) {
    let hay = format!("{} {}", out.raw, out.body);
    for needle in ["XAI_API_KEY", "sk-", "access_token"] {
        assert!(
            !hay.contains(needle),
            "{label} raw body must not contain {needle:?}: {}",
            out.raw
        );
    }
}

fn assert_disabled_body_omits_secret(out: &HttpOut, secret: &str, label: &str) {
    let hay = format!("{} {}", out.raw, out.body);
    assert!(
        !hay.contains(secret),
        "{label} must not echo the in-struct api_key (a set key must not override enabled=false): {}",
        out.raw
    );
    assert_disabled_no_llm_secrets(out, label);
}

fn assert_disabled_error_names_flag(out: &HttpOut, provider: &str, flag: &str) {
    let err = http_error_text(out);
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("disabled") && err.to_ascii_uppercase().contains(flag),
        "{provider} error should say disabled / {flag}: {}",
        out.body
    );
}

fn assert_disabled_provider_complete(out: &HttpOut, provider: &str, flag: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "disabled {provider} POST /chat must not 200: {}",
        out.body
    );
    assert!(
        !out.status.is_server_error(),
        "disabled {provider} POST /chat must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "disabled {provider} POST /chat must 400: {} {}",
        out.status,
        out.body
    );
    assert_disabled_error_names_flag(out, provider, flag);
    assert_disabled_no_llm_secrets(out, &format!("disabled {provider} POST /chat"));
}

fn ndjson_chat_events(raw: &str) -> Vec<Value> {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn assert_disabled_provider_stream(out: &HttpOut, provider: &str, flag: &str) {
    assert!(
        !out.status.is_server_error(),
        "disabled {provider} POST /chat/stream must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_disabled_no_llm_secrets(out, &format!("disabled {provider} POST /chat/stream"));
    if out.status == StatusCode::OK {
        let events = ndjson_chat_events(&out.raw);
        assert!(
            events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("error")),
            "disabled {provider} stream must be an NDJSON error, not a completed assistant turn: {}",
            out.raw
        );
        assert!(
            !events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("done")),
            "disabled {provider} stream must not 200 a successful stream: {}",
            out.raw
        );
        let err_text = events
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("error"))
            .filter_map(|e| e.get("error").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let lower = err_text.to_ascii_lowercase();
        assert!(
            lower.contains("disabled") && err_text.to_ascii_uppercase().contains(flag),
            "{provider} stream error should say disabled / {flag}: {}",
            out.raw
        );
    } else {
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "disabled {provider} POST /chat/stream must 400 (or NDJSON error), not {} {}",
            out.status,
            out.body
        );
        assert_disabled_error_names_flag(out, provider, flag);
    }
}

fn process_llm_key_values() -> Vec<String> {
    ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "CLAUDE_API_KEY"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .collect()
}

async fn call_unset_provider_timed(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    body: Value,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, "POST", path, Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "POST {path} hung >5s — unset openai/claude must not open api.openai.com or api.anthropic.com"
        )
    })
}

fn assert_unset_no_llm_secrets(out: &HttpOut, env_keys: &[String], label: &str) {
    let hay = format!("{} {}", out.raw, out.body);
    for needle in ["sk-", "access_token"] {
        assert!(
            !hay.contains(needle),
            "{label} raw body must not contain {needle:?}: {}",
            out.raw
        );
    }
    for secret in env_keys {
        assert!(
            !hay.contains(secret),
            "{label} must not echo OPENAI_API_KEY / ANTHROPIC_API_KEY / CLAUDE_API_KEY value: {}",
            out.raw
        );
    }
}

fn unset_error_names_missing_key(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("not configured")
        || lower.contains("not set")
        || lower.contains("api key")
        || lower.contains("api_key")
}

/// Claude is runner CLI: missing runner_id 400s "select a runner" before insert.
/// OpenAI is a plane provider and must name the missing API key.
fn unset_error_names_ok(err: &str, provider: &str) -> bool {
    if unset_error_names_missing_key(err) {
        return true;
    }
    provider == "claude" && err.to_ascii_lowercase().contains("select a runner")
}

fn assert_unset_error_names_key(out: &HttpOut, provider: &str) {
    let err = http_error_text(out);
    assert!(
        unset_error_names_ok(&err, provider),
        "{provider} error should say not configured / API key / not set{}",
        if provider == "claude" {
            format!(" (or select a runner): {}", out.body)
        } else {
            format!(": {}", out.body)
        }
    );
}

fn assert_unset_did_not_dial_upstream(out: &HttpOut, label: &str) {
    let hay = format!("{} {}", out.raw, out.body).to_ascii_lowercase();
    let mentions_host = hay.contains("api.openai.com") || hay.contains("api.anthropic.com");
    if !mentions_host {
        return;
    }
    let transport = hay.contains("cannot reach")
        || hay.contains("error sending request")
        || hay.contains("connection")
        || hay.contains("connect")
        || hay.contains("timed out")
        || hay.contains("timeout")
        || hay.contains("dns")
        || hay.contains("error trying to connect");
    assert!(
        !transport,
        "{label} must 400 before opening api.openai.com / api.anthropic.com (transport error after a connect attempt is a fail): {}",
        out.raw
    );
}

fn assert_unset_provider_complete(out: &HttpOut, provider: &str, env_keys: &[String]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "unset {provider} POST /chat must not 200: {}",
        out.body
    );
    assert!(
        !out.status.is_server_error(),
        "unset {provider} POST /chat must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "unset {provider} POST /chat must 400: {} {}",
        out.status,
        out.body
    );
    assert_unset_error_names_key(out, provider);
    assert_unset_no_llm_secrets(out, env_keys, &format!("unset {provider} POST /chat"));
    assert_unset_did_not_dial_upstream(out, &format!("unset {provider} POST /chat"));
}

fn assert_unset_provider_stream(out: &HttpOut, provider: &str, env_keys: &[String]) {
    assert!(
        !out.status.is_server_error(),
        "unset {provider} POST /chat/stream must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_unset_no_llm_secrets(
        out,
        env_keys,
        &format!("unset {provider} POST /chat/stream"),
    );
    assert_unset_did_not_dial_upstream(out, &format!("unset {provider} POST /chat/stream"));
    if out.status == StatusCode::OK {
        let events = ndjson_chat_events(&out.raw);
        assert!(
            events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("error")),
            "unset {provider} stream must be an NDJSON error, not a completed assistant turn: {}",
            out.raw
        );
        assert!(
            !events
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("done")),
            "unset {provider} stream must not 200 a successful stream: {}",
            out.raw
        );
        let err_text = events
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("error"))
            .filter_map(|e| e.get("error").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            unset_error_names_ok(&err_text, provider),
            "{provider} stream error should say not configured / API key / not set{}",
            if provider == "claude" {
                format!(" (or select a runner): {}", out.raw)
            } else {
                format!(": {}", out.raw)
            }
        );
    } else {
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "unset {provider} POST /chat/stream must 400 (or NDJSON error), not {} {}",
            out.status,
            out.body
        );
        assert_unset_error_names_key(out, provider);
    }
}

fn assert_unset_status_providers_unavailable(out: &HttpOut) {
    let providers = out.body["providers"]
        .as_array()
        .unwrap_or_else(|| panic!("control GET /chat/status must have providers: {}", out.body));
    for id in ["openai", "claude"] {
        if let Some(p) = providers.iter().find(|p| p["id"].as_str() == Some(id)) {
            assert_eq!(
                p["available"],
                json!(false),
                "control GET /chat/status {id} must have available=false when the key is unset: {}",
                out.body
            );
        }
    }
}

fn conversation_ids_mentioned(out: &HttpOut) -> Vec<String> {
    let mut ids = Vec::new();
    fn walk(v: &Value, ids: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(id) = map.get("conversation_id").and_then(|x| x.as_str()) {
                    ids.push(id.to_string());
                }
                if let Some(id) = map
                    .get("conversation")
                    .and_then(|c| c.get("id"))
                    .and_then(|x| x.as_str())
                {
                    ids.push(id.to_string());
                }
                for child in map.values() {
                    walk(child, ids);
                }
            }
            Value::Array(arr) => {
                for child in arr {
                    walk(child, ids);
                }
            }
            _ => {}
        }
    }
    walk(&out.body, &mut ids);
    for ev in ndjson_chat_events(&out.raw) {
        walk(&ev, &mut ids);
    }
    ids.retain(|id| Uuid::parse_str(id).is_ok());
    ids.sort();
    ids.dedup();
    ids
}

fn assert_conversations_unchanged(
    snap: &HttpOut,
    after: &HttpOut,
    unique_contents: &[&str],
    what: &str,
) {
    assert_eq!(
        after.status,
        StatusCode::OK,
        "{what} GET /conversations: {} {}",
        after.status,
        after.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);
    let after_rows = listed_conversation_id_titles(&after.body);
    assert_eq!(
        after_rows, snap_rows,
        "{what} GET /conversations must match the snapshot (no new conversation_id): snap={snap_rows:?} after={after_rows:?} body={}",
        after.body
    );
    let hay = format!("{} {}", after.raw, after.body);
    for content in unique_contents {
        assert!(
            !hay.contains(content),
            "{what} list must not persist the rejected turn as a title: {content} in {}",
            after.body
        );
    }
}

async fn assert_new_conversation_ids_are_404(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    snap_rows: &[(String, String)],
    after: &HttpOut,
    posts: &[&HttpOut],
    what: &str,
) {
    let snap_ids: Vec<&str> = snap_rows.iter().map(|(id, _)| id.as_str()).collect();
    let mut extra: Vec<String> = listed_conversation_id_titles(&after.body)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !snap_ids.contains(&id.as_str()))
        .collect();
    for post in posts {
        extra.extend(
            conversation_ids_mentioned(post)
                .into_iter()
                .filter(|id| !snap_ids.contains(&id.as_str())),
        );
    }
    extra.sort();
    extra.dedup();
    for id in extra {
        let got = call(
            app,
            "GET",
            &format!("/api/workspaces/{ws}/chat/conversations/{id}"),
            Some(jar),
            None,
        )
        .await;
        assert_eq!(
            got.status,
            StatusCode::NOT_FOUND,
            "{what} GET /conversations/{id} for an id that appeared only after the 400s must be 404: {} {}",
            got.status,
            got.body
        );
    }
}

async fn assert_unique_turns_not_persisted(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    listed: &HttpOut,
    unique_contents: &[&str],
    what: &str,
) {
    for (id, _title) in listed_conversation_id_titles(&listed.body) {
        let got = call(
            app,
            "GET",
            &format!("/api/workspaces/{ws}/chat/conversations/{id}"),
            Some(jar),
            None,
        )
        .await;
        if got.status != StatusCode::OK {
            continue;
        }
        let hay = format!("{} {}", got.raw, got.body);
        for content in unique_contents {
            assert!(
                !hay.contains(content),
                "{what} must not persist a user or assistant turn `{content}` on {id}: {}",
                got.body
            );
        }
    }
}

fn listed_conversation_id_titles(body: &Value) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations array: {body}"))
        .iter()
        .map(|c| {
            let id = c["id"]
                .as_str()
                .unwrap_or_else(|| panic!("conversation id: {c}"))
                .to_string();
            let title = c["title"]
                .as_str()
                .unwrap_or_else(|| panic!("conversation title: {c}"))
                .to_string();
            (id, title)
        })
        .collect();
    rows.sort();
    rows
}

fn assert_conversations_not_a_member(out: &HttpOut, label: &str, leaks: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not be 200: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{label} must not be 201: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    if let Some(arr) = out.body.get("conversations").and_then(|v| v.as_array()) {
        assert!(
            arr.is_empty(),
            "{label} must not include conversations: {}",
            out.body
        );
    }
    if let Some(id) = out.body.get("id").and_then(|v| v.as_str()) {
        panic!(
            "{label} must not return a conversation id {id}: {}",
            out.body
        );
    }
    if let Some(conv) = out.body.get("conversation") {
        if !conv.is_null() {
            panic!("{label} must not return conversation: {}", out.body);
        }
    }
    for secret in leaks {
        assert!(
            !out.raw.contains(secret),
            "{label} body must not contain {secret:?}: {}",
            out.raw
        );
        assert!(
            !out.body.to_string().contains(secret),
            "{label} json must not contain {secret:?}: {}",
            out.body
        );
    }
}

/// GET /chat/conversations and POST /chat/conversations on another
/// workspace must 403 not-a-member and must not leak or insert. Distinct
/// from `chat_status_create_list_delete_other_user_forbidden` (by-id GET
/// as B on A's path) and `chat_conversation_user_workspace_scope`
/// (same-user A/B conversation_id and teammate GET/DELETE/complete).
/// Filter `-- conversations_list_create_other_ws`.
#[tokio::test]
async fn chat_conversations_list_create_other_ws_are_403_and_do_not_insert() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must have distinct workspaces");

    let a_secret_title = format!("unique-secret-A-{}", Uuid::new_v4());
    let created_a = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "title": a_secret_title })),
    )
    .await;
    assert_eq!(
        created_a.status,
        StatusCode::CREATED,
        "A POST /conversations: {}",
        created_a.body
    );
    let a_cid = created_a.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created_a.body))
        .to_string();
    assert_eq!(
        created_a.body["conversation"]["title"].as_str(),
        Some(a_secret_title.as_str()),
        "A create title: {}",
        created_a.body
    );

    let snap = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /conversations: {}",
        snap.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);
    let snap_count = snap.body["count"]
        .as_u64()
        .unwrap_or_else(|| snap_rows.len() as u64);
    assert_eq!(
        snap_count as usize,
        snap_rows.len(),
        "A snapshot count must match conversations array: {}",
        snap.body
    );
    assert!(
        snap_rows
            .iter()
            .any(|(id, title)| id == &a_cid && title == &a_secret_title),
        "A snapshot must include planted {a_cid} / {a_secret_title} so the leak check is not vacuous: {}",
        snap.body
    );

    let list_as_b = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&b_jar),
        None,
    )
    .await;
    assert_conversations_not_a_member(
        &list_as_b,
        "B GET A's /conversations",
        &[a_cid.as_str(), a_secret_title.as_str()],
    );

    let b_stolen_title = format!("unique-from-B-{}", Uuid::new_v4());
    let create_as_b = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&b_jar),
        Some(json!({ "title": b_stolen_title })),
    )
    .await;
    assert_conversations_not_a_member(
        &create_as_b,
        "B POST A's /conversations",
        &[a_cid.as_str(), a_secret_title.as_str()],
    );
    assert_ne!(
        create_as_b.status,
        StatusCode::CREATED,
        "B POST A's /conversations must not 201 with a new id: {}",
        create_as_b.body
    );

    let after = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "A GET /conversations after B: {}",
        after.body
    );
    let after_rows = listed_conversation_id_titles(&after.body);
    let after_count = after.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_rows.len() as u64);
    assert_eq!(
        after_count, snap_count,
        "A's list count must still match the snapshot (no extra row): snap={snap_count} after={after_count} body={}",
        after.body
    );
    assert_eq!(
        after_rows, snap_rows,
        "A's list must still match the snapshot (no extra row, original title unchanged): snap={snap_rows:?} after={after_rows:?} body={}",
        after.body
    );
    assert!(
        after_rows
            .iter()
            .all(|(id, title)| { !(id == &a_cid && title != &a_secret_title) }),
        "original title of {a_cid} must be unchanged: {}",
        after.body
    );
    assert!(
        !after.raw.contains(&b_stolen_title)
            && after_rows.iter().all(|(_, title)| title != &b_stolen_title),
        "B's stolen title must not appear on A's list: {}",
        after.body
    );

    let b_list = call(
        &app,
        "GET",
        &chat_conversations_path(&b_ws),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_list.status,
        StatusCode::OK,
        "B GET B /conversations must be 200 so this is not a tautology that every conversations call is 403: {}",
        b_list.body
    );
    let b_hay = format!("{} {}", b_list.raw, b_list.body);
    assert!(
        !b_hay.contains(&a_cid) && !b_hay.contains(&a_secret_title),
        "B GET B /conversations must not include A's id or title: {}",
        b_list.body
    );
    let b_rows = listed_conversation_id_titles(&b_list.body);
    assert!(
        b_rows
            .iter()
            .all(|(id, title)| id != &a_cid && title != &a_secret_title),
        "B GET B /conversations must not list A's conversation: {}",
        b_list.body
    );

    let b_own_title = format!("unique-own-B-{}", Uuid::new_v4());
    let created_b = call(
        &app,
        "POST",
        &chat_conversations_path(&b_ws),
        Some(&b_jar),
        Some(json!({ "title": b_own_title })),
    )
    .await;
    assert_eq!(
        created_b.status,
        StatusCode::CREATED,
        "control: B POST B /conversations must be 201 so this is not a tautology that every conversations call is 403: {}",
        created_b.body
    );
    let b_own_id = created_b.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("B conversation.id: {}", created_b.body))
        .to_string();
    assert_ne!(b_own_id, a_cid, "B must mint a distinct conversation id");
    assert_eq!(
        created_b.body["conversation"]["title"].as_str(),
        Some(b_own_title.as_str()),
        "control B create title: {}",
        created_b.body
    );

    let b_got = call(
        &app,
        "GET",
        &chat_conversation_path(&b_ws, &b_own_id),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_got.status,
        StatusCode::OK,
        "control: B GET B /conversations/{{id}} must be 200 so this is not a tautology that every conversations call is 403: {}",
        b_got.body
    );
    assert_eq!(
        b_got.body["conversation"]["id"].as_str(),
        Some(b_own_id.as_str()),
        "control B GET id: {}",
        b_got.body
    );
    assert_eq!(
        b_got.body["conversation"]["title"].as_str(),
        Some(b_own_title.as_str()),
        "control B GET title: {}",
        b_got.body
    );

    let b_list_after = call(
        &app,
        "GET",
        &chat_conversations_path(&b_ws),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_list_after.status,
        StatusCode::OK,
        "control: B GET B /conversations after own create: {}",
        b_list_after.body
    );
    let b_after_rows = listed_conversation_id_titles(&b_list_after.body);
    assert!(
        b_after_rows
            .iter()
            .any(|(id, title)| id == &b_own_id && title == &b_own_title),
        "control: B GET B /conversations must include B's own conversation: {}",
        b_list_after.body
    );
    assert!(
        b_after_rows
            .iter()
            .all(|(id, title)| id != &a_cid && title != &a_secret_title),
        "control: B's list must still omit A's conversation: {}",
        b_list_after.body
    );
}

/// GET /chat/conversations lists only the caller's threads. A teammate who
/// is a member (200, not membership 403) must not see another member's
/// conversation ids or titles. Teammate GET/DELETE of a live id stays 404
/// (`chat_conversation_user_workspace_scope`). Non-member list/create 403
/// stays on `chat_conversations_list_create_other_ws_are_403_and_do_not_insert`.
/// Filter `-- conversations_list_teammate`.
#[tokio::test]
async fn chat_conversations_list_teammate_omits_other_users() {
    let app = app().await;
    let tag = Uuid::new_v4();
    let title_a = format!("unique-secret-A-{tag}");
    let title_g = format!("unique-secret-G-{tag}");

    // 1. Owner A POST /conversations {title: unique-secret-A} 201.
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let created_a = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "title": title_a })),
    )
    .await;
    assert_eq!(
        created_a.status,
        StatusCode::CREATED,
        "A POST /conversations: {}",
        created_a.body
    );
    let cid_a = created_a.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created_a.body))
        .to_string();
    assert_eq!(
        created_a.body["conversation"]["title"].as_str(),
        Some(title_a.as_str()),
        "snapshot title: {}",
        created_a.body
    );

    // 2. A invites unique guest G (role member). G logs in.
    let (g_email, _g_signup_jar, _g_ws) = signup_and_login(&app).await;
    let invited = call(
        &app,
        "POST",
        &invites_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "email": g_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        invited.status,
        StatusCode::CREATED,
        "invite teammate: {}",
        invited.body
    );
    assert_eq!(
        invited.body["added_existing_user"], true,
        "existing user must join immediately: {}",
        invited.body
    );
    let (g_login, g_jar) = login(&app, &g_email, TEST_PASSWORD).await;
    assert_eq!(g_login.status, StatusCode::OK, "G login: {}", g_login.body);

    // G's cookie on A's GET /conversations is 200. Must not include cid_a
    // or unique-secret-A.
    let g_list_empty = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&g_jar),
        None,
    )
    .await;
    assert_eq!(
        g_list_empty.status,
        StatusCode::OK,
        "teammate GET /conversations must be 200 (member, not 403): {}",
        g_list_empty.body
    );
    assert_ne!(
        g_list_empty.status,
        StatusCode::FORBIDDEN,
        "teammate list must not 403: {}",
        g_list_empty.body
    );
    let g_convs_empty = g_list_empty.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", g_list_empty.body));
    assert!(
        g_convs_empty
            .iter()
            .all(|c| c["id"].as_str() != Some(cid_a.as_str())),
        "teammate list must not include cid_a {cid_a}: {}",
        g_list_empty.body
    );
    let g_empty_hay = format!("{} {}", g_list_empty.raw, g_list_empty.body);
    assert!(
        !g_empty_hay.contains(&cid_a) && !g_empty_hay.contains(&title_a),
        "teammate list must not include cid_a or unique-secret-A: {}",
        g_list_empty.body
    );
    assert!(
        g_convs_empty.is_empty(),
        "teammate with no threads must list none, not the owner's: {}",
        g_list_empty.body
    );

    // G POST /conversations {title: unique-secret-G} 201.
    let created_g = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&g_jar),
        Some(json!({ "title": title_g })),
    )
    .await;
    assert_eq!(
        created_g.status,
        StatusCode::CREATED,
        "G POST /conversations: {}",
        created_g.body
    );
    let cid_g = created_g.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("G conversation.id: {}", created_g.body))
        .to_string();
    assert_ne!(cid_g, cid_a, "G must mint a distinct conversation id");

    // A's GET list still has cid_a and must not include G's id or unique-secret-G.
    let a_list = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        a_list.status,
        StatusCode::OK,
        "A list after G create: {}",
        a_list.body
    );
    let a_convs = a_list.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", a_list.body));
    assert!(
        a_convs
            .iter()
            .any(|c| c["id"].as_str() == Some(cid_a.as_str())),
        "A list must still include cid_a {cid_a}: {}",
        a_list.body
    );
    assert!(
        a_convs
            .iter()
            .all(|c| c["id"].as_str() != Some(cid_g.as_str())),
        "A list must not include G's id {cid_g}: {}",
        a_list.body
    );
    let a_hay = format!("{} {}", a_list.raw, a_list.body);
    assert!(
        a_hay.contains(&title_a),
        "A list must still include unique-secret-A: {}",
        a_list.body
    );
    assert!(
        !a_hay.contains(&cid_g) && !a_hay.contains(&title_g),
        "A list must not include G's id or unique-secret-G: {}",
        a_list.body
    );

    // G's GET list has only G's id.
    let g_list = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&g_jar),
        None,
    )
    .await;
    assert_eq!(
        g_list.status,
        StatusCode::OK,
        "G list after create: {}",
        g_list.body
    );
    let g_convs = g_list.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", g_list.body));
    assert_eq!(
        g_convs.len(),
        1,
        "G list must have only G's conversation: {}",
        g_list.body
    );
    assert_eq!(
        g_convs[0]["id"].as_str(),
        Some(cid_g.as_str()),
        "G list must have only G's id: {}",
        g_list.body
    );
    assert_eq!(
        g_convs[0]["title"].as_str(),
        Some(title_g.as_str()),
        "G list title: {}",
        g_list.body
    );
    let g_hay = format!("{} {}", g_list.raw, g_list.body);
    assert!(
        !g_hay.contains(&cid_a) && !g_hay.contains(&title_a),
        "G list must not include A's id or unique-secret-A: {}",
        g_list.body
    );

    // 3. Teammate GET/DELETE /conversations/{cid_a} is 404. Body must not
    //    include unique-secret-A.
    let teammate_get = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &cid_a),
        Some(&g_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &teammate_get,
        &[title_a.as_str()],
        "teammate GET /conversations/{cid_a}",
    );
    let teammate_delete = call(
        &app,
        "DELETE",
        &chat_conversation_path(&a_ws, &cid_a),
        Some(&g_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &teammate_delete,
        &[title_a.as_str()],
        "teammate DELETE /conversations/{cid_a}",
    );

    // 4. Control: A's GET list still 200 with cid_a so this is not a tautology
    //    that every list is empty. B (non-member) GET list on A's path stays 403.
    let a_control = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        a_control.status,
        StatusCode::OK,
        "control A list: {}",
        a_control.body
    );
    let a_control_convs = a_control.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", a_control.body));
    assert!(
        a_control_convs
            .iter()
            .any(|c| c["id"].as_str() == Some(cid_a.as_str())),
        "control A list must still include cid_a {cid_a}: {}",
        a_control.body
    );
    assert!(
        a_control_convs
            .iter()
            .all(|c| c["id"].as_str() != Some(cid_g.as_str())),
        "control A list must not include G's id {cid_g}: {}",
        a_control.body
    );
    let a_control_hay = format!("{} {}", a_control.raw, a_control.body);
    assert!(
        a_control_hay.contains(&title_a),
        "control A list must still include unique-secret-A: {}",
        a_control.body
    );
    assert!(
        !a_control_hay.contains(&title_g),
        "control A list must not include unique-secret-G: {}",
        a_control.body
    );

    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let b_list = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_list.status,
        StatusCode::FORBIDDEN,
        "non-member GET list on A's path must stay 403: {} {}",
        b_list.status,
        b_list.body
    );
    let b_hay = format!("{} {}", b_list.raw, b_list.body);
    assert!(
        !b_hay.contains(&cid_a) && !b_hay.contains(&title_a) && !b_hay.contains(&title_g),
        "non-member 403 must not leak conversation ids or titles: {}",
        b_list.body
    );
}

/// DELETE is a hard delete. GET /conversations/{id} and POST /chat complete
/// with that `conversation_id` must 404 and must not recreate the row
/// (client-supplied UUID) or insert `chat_messages` for it.
/// `chat_status_create_list_delete_other_user_forbidden` DELETEs then ends —
/// this lock continues. Stream stays on
/// `chat_stream_deleted_conversation_stays_gone` (`-- deleted_conversation`
/// hits both). Other-ws / teammate IDOR stays on
/// `chat_conversation_user_workspace_scope`.
#[tokio::test]
async fn chat_deleted_conversation_stays_gone() {
    let (app, state) = app_with_state().await;
    let tag = Uuid::new_v4();
    let title = format!("unique-secret-{tag}");
    let unique_hi = format!("unique-hi-{tag}");
    let other_title = format!("other-title-{tag}");
    let secrets = [title.as_str(), unique_hi.as_str()];

    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");

    // 1. Owner POST /conversations {title: unique-secret} 201. Snapshot cid.
    //    DELETE /conversations/{cid} 200 deleted=true.
    let created = call(
        &app,
        "POST",
        &chat_conversations_path(&ws),
        Some(&jar),
        Some(json!({ "title": title })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "POST /conversations: {}",
        created.body
    );
    let cid = created.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created.body))
        .to_string();
    let cid_uuid = Uuid::parse_str(&cid).expect("cid");

    let deleted = call(
        &app,
        "DELETE",
        &chat_conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body["deleted"], true);

    // 2. Same cookie GET /conversations/{cid} is 404 conversation (not 200
    //    with the title). GET list must not include cid or unique-secret.
    let got = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_conversation_not_found(&got, &secrets, "owner GET deleted /conversations/{cid}");
    let listed = call(&app, "GET", &chat_conversations_path(&ws), Some(&jar), None).await;
    assert_deleted_conversation_absent_from_list(&listed, &cid, &secrets, "after DELETE");
    assert_deleted_conversation_not_in_db(&state, cid_uuid, ws_uuid, user_id, "after DELETE").await;

    // 3. POST /chat {conversation_id: cid, …, provider: ollama|grok} is 404
    //    conversation (not 200/201). Pair a runner so skipping
    //    ensure_conversation would enqueue chat_turn instead of 400
    //    "select a runner".
    let runner = create_paired_online_runner(&app, &jar, &ws).await;
    for provider in ["grok", "ollama"] {
        let complete = post_chat_complete_timed(
            &app,
            &jar,
            &ws,
            json!({
                "messages": [{ "role": "user", "content": unique_hi }],
                "provider": provider,
                "conversation_id": cid,
                "runner_id": runner,
            }),
        )
        .await;
        assert_conversation_not_found(
            &complete,
            &secrets,
            &format!("POST /chat {provider} conversation_id=deleted"),
        );
        assert_ne!(
            complete.status,
            StatusCode::CREATED,
            "POST /chat {provider} must not 201-a-new-row-with-the-same-id: {}",
            complete.body
        );
    }

    let after_complete = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &after_complete,
        &secrets,
        "GET /conversations/{cid} after complete",
    );
    let listed_after = call(&app, "GET", &chat_conversations_path(&ws), Some(&jar), None).await;
    assert_deleted_conversation_absent_from_list(&listed_after, &cid, &secrets, "after complete");
    assert_deleted_conversation_not_in_db(&state, cid_uuid, ws_uuid, user_id, "after complete")
        .await;
    assert_no_chat_turn(&state, ws_uuid, "complete vs deleted conversation").await;

    // 5. Control: POST /conversations {title: other} still 201 a *new* id ≠ cid
    //    so this is not a tautology that every chat POST 404s.
    let control = call(
        &app,
        "POST",
        &chat_conversations_path(&ws),
        Some(&jar),
        Some(json!({ "title": other_title })),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::CREATED,
        "control POST /conversations: {}",
        control.body
    );
    let new_id = control.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control conversation.id: {}", control.body));
    assert_ne!(
        new_id, cid,
        "control create must mint a new id, not resurrect {cid}: {}",
        control.body
    );
    let control_get = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, new_id),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        control_get.status,
        StatusCode::OK,
        "control GET new conversation: {}",
        control_get.body
    );
    assert_eq!(
        control_get.body["conversation"]["title"].as_str(),
        Some(other_title.as_str()),
        "control title: {}",
        control_get.body
    );
}

/// POST /chat and /chat/stream reject role=tool|function|admin|root with
/// 400 `invalid message role`. Must not 200 an assistant turn that saw the
/// planted secret, persist it on GET /conversations, or enqueue a chat_turn.
/// Distinct from empty-messages, max-32k, and unknown-provider.
/// Filter `-- chat_invalid_role`.
#[tokio::test]
async fn chat_invalid_role_rejected_no_persist() {
    let (_base, mut state) = app_with_state().await;
    // Plane-enabled so /chat/stream reaches prepare_chat_pairs (the role
    // gate). Disabled ollama 400s earlier as GROK/OLLAMA_ENABLED and would
    // make every stream 400 a tautology.
    state.ollama.enabled = true;
    state.ollama.base_url = "http://127.0.0.1:9".into();
    let app = build_router().with_state(state.clone());

    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let chat = chat_complete_path(&ws);
    let stream = format!("{chat}/stream");
    let conversations = chat_conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);

    let mut secrets: Vec<String> = Vec::new();
    for role in ["tool", "function", "admin", "root"] {
        let secret = format!("invalid-role-{role}-{}", Uuid::new_v4());
        let body = json!({
            "messages": [
                { "role": role, "content": secret },
                { "role": "user", "content": "hi" }
            ],
            "provider": "ollama",
            "runner_id": runner_id,
        });
        let complete = call_chat_invalid_role_timed(&app, &jar, &chat, body.clone()).await;
        assert_invalid_message_role_complete(&complete, role, &secret, "POST /chat");
        let streamed = call_chat_invalid_role_timed(&app, &jar, &stream, body).await;
        assert_invalid_message_role_stream(&streamed, role, &secret, "POST /chat/stream");
        secrets.push(secret);
    }

    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(&snap, &after, &secret_refs, "after invalid-role POSTs");
    assert_eq!(
        listed_conversation_id_titles(&after.body).len(),
        snap_rows.len(),
        "GET /conversations count must be unchanged after invalid-role reject: snap={} after={} body={}",
        snap_rows.len(),
        listed_conversation_id_titles(&after.body).len(),
        after.body
    );
    assert_unique_turns_not_persisted(
        &app,
        &jar,
        &ws,
        &after,
        &secret_refs,
        "after invalid-role POSTs",
    )
    .await;
    assert_no_chat_turn(&state, ws_uuid, "invalid message role").await;

    // Control: role=user and role=system + later role=user may 400 from
    // no provider / select-a-runner — must not 400 as invalid role.
    let user_only = call_chat_invalid_role_timed(
        &app,
        &jar,
        &chat,
        json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role(&user_only, "control role=user POST /chat");
    let system_then_user = call_chat_invalid_role_timed(
        &app,
        &jar,
        &chat,
        json!({
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role(
        &system_then_user,
        "control role=system + role=user POST /chat",
    );
    let user_stream = call_chat_invalid_role_timed(
        &app,
        &jar,
        &stream,
        json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role(&user_stream, "control role=user POST /chat/stream");
    let system_stream = call_chat_invalid_role_timed(
        &app,
        &jar,
        &stream,
        json!({
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role(
        &system_stream,
        "control role=system + role=user POST /chat/stream",
    );
}

async fn call_chat_invalid_role_timed(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    body: Value,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, "POST", path, Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "POST {path} hung >5s — invalid roles must 400 before runner wait / NDJSON / persist"
        )
    })
}

fn assert_invalid_message_role_complete(out: &HttpOut, role: &str, secret: &str, what: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} role={role} must not 200 an assistant reply that saw the secret: {}",
        out.body
    );
    assert!(
        !out.status.is_server_error(),
        "{what} role={role} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} role={role} must be 400 invalid message role: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("invalid message role"),
        "{what} role={role} 400 must say invalid message role: {}",
        out.body
    );
    assert!(
        err.contains(role),
        "{what} role={role} 400 must name the rejected role: {}",
        out.body
    );
    if let Some(got) = out.body["message"]["role"].as_str() {
        assert_ne!(
            got, "assistant",
            "{what} role={role} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
    let hay = format!("{} {}", out.raw, out.body);
    if let Some(content) = out.body["message"]["content"].as_str() {
        assert!(
            !content.contains(secret),
            "{what} role={role} assistant content must not include the planted secret: {}",
            out.body
        );
    }
    assert!(
        !hay.contains("\"type\":\"done\"") && !hay.contains("\"type\": \"done\""),
        "{what} role={role} must not stream a completed turn: {}",
        out.raw
    );
}

fn assert_invalid_message_role_stream(out: &HttpOut, role: &str, secret: &str, what: &str) {
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} role={role} must be 400, not 200 NDJSON ({ctype}): {}",
        out.raw
    );
    assert!(
        !out.status.is_server_error(),
        "{what} role={role} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} role={role} must be 400 invalid message role, not 200 NDJSON: {} {}",
        out.status,
        out.body
    );
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} role={role} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("invalid message role"),
        "{what} role={role} 400 must say invalid message role: {}",
        out.body
    );
    assert!(
        err.contains(role),
        "{what} role={role} 400 must name the rejected role: {}",
        out.body
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} role={role} must not stream type=done: {}",
        out.raw
    );
    if let Some(content) = out.body["message"]["content"].as_str() {
        assert!(
            !content.contains(secret),
            "{what} role={role} must not return the planted secret as assistant text: {}",
            out.body
        );
    }
}

fn assert_not_invalid_message_role(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} member cookie must still authenticate: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} member cookie must still be allowed: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        !err.contains("invalid message role"),
        "{what} must not 400 as invalid role (400 from no provider/model/runner is ok): {}",
        out.body
    );
}

/// POST /chat and /chat/stream reject a user line over 32k (`message too long`)
/// before `ensure_conversation` / persist / `run_on_runner`. Distinct from
/// empty-messages, conversation title-max-72, launch-spec-max-32k, and
/// invalid-role. Filter `-- chat_message_max`.
#[tokio::test]
async fn chat_message_max_32k_rejected_no_persist() {
    let (_base, mut state) = app_with_state().await;
    // Plane-enabled so /chat/stream reaches prepare_chat_pairs (the 32k
    // gate). Disabled ollama 400s earlier as OLLAMA_ENABLED and would make
    // every stream 400 a tautology that misses a dropped length check.
    state.ollama.enabled = true;
    state.ollama.base_url = "http://127.0.0.1:9".into();
    let app = build_router().with_state(state.clone());

    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let chat = chat_complete_path(&ws);
    let stream = format!("{chat}/stream");
    let conversations = chat_conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_rows = listed_conversation_id_titles(&snap.body);
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);
    assert!(
        !conversation_list_contains_blob(&snap.body, &huge),
        "pre-call GET /conversations must not already contain the 32001-char string"
    );

    let over_body = json!({
        "messages": [{ "role": "user", "content": huge }],
        "provider": "ollama",
        "runner_id": runner_id,
    });
    let complete = call_chat_message_max_timed(&app, &jar, &chat, over_body.clone()).await;
    assert_message_too_long_complete(&complete, &huge, "POST /chat");
    let streamed = call_chat_message_max_timed(&app, &jar, &stream, over_body).await;
    assert_message_too_long_stream(&streamed, &huge, "POST /chat/stream");

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_conversations_unchanged(&snap, &after, &[&huge], "after 32001-char POSTs");
    assert_eq!(
        listed_conversation_id_titles(&after.body).len(),
        snap_rows.len(),
        "GET /conversations count must be unchanged after 32001-char reject: snap={} after={} body={}",
        snap_rows.len(),
        listed_conversation_id_titles(&after.body).len(),
        after.body
    );
    let leaked_title = title_from_overlong_user(&huge);
    assert!(
        !listed_conversation_id_titles(&after.body)
            .iter()
            .any(|(_, title)| title == &huge || title == &leaked_title),
        "GET /conversations must not include a new row whose title is the 32001-char string (or its 72-char clip): {}",
        after.body
    );
    assert_unique_turns_not_persisted(&app, &jar, &ws, &after, &[&huge], "after 32001-char POSTs")
        .await;
    assert_no_chat_turn(&state, ws_uuid, "32001-char chat message").await;

    let created = call(
        &app,
        "POST",
        &conversations,
        Some(&jar),
        Some(json!({ "title": format!("short-thread-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "setup POST /conversations: {}",
        created.body
    );
    let cid = created.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created.body))
        .to_string();
    let existing = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        existing.status,
        StatusCode::OK,
        "setup GET /conversations/{{id}}: {}",
        existing.body
    );
    let snap_msgs = conversation_message_count(&existing.body);

    let resume = call_chat_message_max_timed(
        &app,
        &jar,
        &chat,
        json!({
            "conversation_id": cid,
            "messages": [{ "role": "user", "content": huge }],
            "provider": "ollama",
            "runner_id": runner_id,
        }),
    )
    .await;
    assert_message_too_long_complete(
        &resume,
        &huge,
        "POST /chat with conversation_id + 32001-char user line",
    );
    let got = call(
        &app,
        "GET",
        &chat_conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET /conversations/{{id}} after 32001-char resume: {}",
        got.body
    );
    assert_eq!(
        conversation_message_count(&got.body),
        snap_msgs,
        "existing thread must not gain a 32001-char user row: {}",
        got.body
    );
    let hay = format!("{} {}", got.raw, got.body);
    assert!(
        !hay.contains(&huge),
        "GET /conversations/{{id}} messages must not include the 32001-char string"
    );
    assert_no_chat_turn(&state, ws_uuid, "32001-char resume on existing thread").await;

    // Control: 32_000 chars may proceed (400 from no runner / no model is
    // ok) — must not 400 as too long, or every complete 400 is a tautology.
    let at_cap = "y".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);
    let control_complete = call_chat_message_max_timed(
        &app,
        &jar,
        &chat,
        json!({
            "messages": [{ "role": "user", "content": at_cap }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_message_too_long(&control_complete, "control 32_000-char POST /chat");
    let control_stream = call_chat_message_max_timed(
        &app,
        &jar,
        &stream,
        json!({
            "messages": [{ "role": "user", "content": at_cap }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_message_too_long(&control_stream, "control 32_000-char POST /chat/stream");
}

async fn call_chat_message_max_timed(
    app: &axum::Router,
    jar: &CookieJar,
    path: &str,
    body: Value,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, "POST", path, Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "POST {path} hung >5s — over-32k messages must 400 before runner wait / NDJSON / persist"
        )
    })
}

fn title_from_overlong_user(text: &str) -> String {
    let t = text.trim().replace('\n', " ");
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= 72 {
        t
    } else {
        format!("{}…", chars.into_iter().take(69).collect::<String>())
    }
}

fn conversation_list_contains_blob(body: &Value, blob: &str) -> bool {
    listed_conversation_id_titles(body)
        .iter()
        .any(|(id, title)| id == blob || title == blob || title.contains(blob))
}

fn assert_message_too_long_complete(out: &HttpOut, huge: &str, what: &str) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} must not 200 an assistant reply that saw the 32001-char line: status={}",
        out.status
    );
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 message too long: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("too long")
            || err.contains("32k")
            || err.contains("32000")
            || err.contains("32_000"),
        "{what} 400 must be message too long (max 32k bytes): {}",
        out.body
    );
    assert!(
        !out.raw.contains(huge),
        "{what} 400 body must not echo the 32001-char user line: {}",
        out.status
    );
    if let Some(got) = out.body["message"]["role"].as_str() {
        assert_ne!(
            got, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream a completed turn: {}",
        out.status
    );
}

fn assert_message_too_long_stream(out: &HttpOut, huge: &str, what: &str) {
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} must be 400, not 200 NDJSON ({ctype}): {}",
        out.status
    );
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 message too long, not 200 NDJSON: {} {}",
        out.status,
        out.body
    );
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.status
    );
    assert_message_too_long_complete(out, huge, what);
}

fn assert_not_message_too_long(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} member cookie must still authenticate: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} member cookie must still be allowed: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        !err.contains("too long")
            && !err.contains("32k")
            && !err.contains("32_000")
            && !err.contains("32000"),
        "{what} must not 400 as too long (400 from no provider/model/runner is ok): {}",
        out.body
    );
}

fn chat_conversations_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations")
}

fn chat_conversation_path(ws: &str, cid: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations/{cid}")
}

fn chat_complete_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat")
}

fn conversation_message_count(body: &Value) -> usize {
    body["messages"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("messages array: {body}"))
}

fn conversation_message_contents(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("messages array: {body}"))
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// Ownership 404 — not membership 403. A member of the path workspace who
/// does not own this `conversation_id` (other workspace or teammate) must
/// not see title/history and must not delete or append.
fn assert_conversation_not_found(out: &HttpOut, secrets: &[&str], what: &str) {
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "{what} must be 404 conversation (not 200 with that thread, not membership 403): {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out).to_ascii_lowercase();
    assert!(
        err.contains("conversation"),
        "{what} 404 must name conversation: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} must not return 200 with another conversation: {}",
        out.body
    );
    assert!(
        out.body.get("messages").is_none() || out.body["messages"].is_null(),
        "{what} must not include messages: {}",
        out.body
    );
    assert!(
        out.body.get("conversation").is_none() || out.body["conversation"].is_null(),
        "{what} must not include conversation: {}",
        out.body
    );
    assert_ne!(
        out.body.get("deleted"),
        Some(&json!(true)),
        "{what} must not report deleted: {}",
        out.body
    );
    let hay = format!("{} {}", out.raw, out.body);
    for secret in secrets {
        assert!(
            !hay.contains(secret),
            "{what} must not leak stored title or history `{secret}`: {}",
            out.body
        );
    }
}

fn assert_deleted_conversation_absent_from_list(
    listed: &HttpOut,
    cid: &str,
    secrets: &[&str],
    what: &str,
) {
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "{what} GET /conversations: {} {}",
        listed.status,
        listed.body
    );
    let convs = listed.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("{what} conversations: {}", listed.body));
    assert!(
        convs.iter().all(|c| c["id"].as_str() != Some(cid)),
        "{what} list must not include deleted {cid}: {}",
        listed.body
    );
    let hay = format!("{} {}", listed.raw, listed.body);
    for secret in secrets {
        assert!(
            !hay.contains(secret),
            "{what} list must not include `{secret}`: {}",
            listed.body
        );
    }
}

async fn assert_deleted_conversation_not_in_db(
    state: &AppState,
    conversation_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
    what: &str,
) {
    let row =
        fleet_db::repos::chats::get_conversation(&state.db, conversation_id, workspace_id, user_id)
            .await
            .expect("get_conversation");
    assert!(
        row.is_none(),
        "{what} must not insert chat_conversations id={conversation_id}: {row:?}"
    );
    let msgs = fleet_db::repos::chats::list_messages(&state.db, conversation_id)
        .await
        .expect("list_messages");
    assert!(
        msgs.is_empty(),
        "{what} must not insert chat_messages for {conversation_id}: {msgs:?}"
    );
}

fn assert_owner_conversation_snapshot(
    got: &HttpOut,
    cid: &str,
    title: &str,
    message_count: usize,
    stored_history: &str,
    unique_hi: &str,
    what: &str,
) {
    assert_eq!(
        got.status,
        StatusCode::OK,
        "{what} owner GET /conversations/{{id}} must stay 200: {} {}",
        got.status,
        got.body
    );
    assert_eq!(
        got.body["conversation"]["id"].as_str(),
        Some(cid),
        "{what} conversation id: {}",
        got.body
    );
    assert_eq!(
        got.body["conversation"]["title"].as_str(),
        Some(title),
        "{what} title must stay the owner's secret: {}",
        got.body
    );
    assert_eq!(
        conversation_message_count(&got.body),
        message_count,
        "{what} message count must be unchanged: {}",
        got.body
    );
    let contents = conversation_message_contents(&got.body);
    assert!(
        contents.iter().any(|c| c == stored_history),
        "{what} must keep the owner's planted history: {:?}",
        contents
    );
    assert!(
        contents.iter().all(|c| !c.contains(unique_hi)),
        "{what} must not persist a user row with the complete content: {:?}",
        contents
    );
}

/// Owner cookie creates a paired, heartbeating runner so a skipped
/// `ensure_conversation` on POST /chat (grok|ollama + runner_id) would
/// enqueue `chat_turn` instead of 400 "select a runner".
async fn create_paired_online_runner(app: &axum::Router, jar: &CookieJar, ws: &str) -> Uuid {
    let name = format!("chat-scope-{}", Uuid::new_v4());
    let created = call(
        app,
        "POST",
        &hosts_path(ws),
        Some(jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert!(
        created.status == StatusCode::OK || created.status == StatusCode::CREATED,
        "create host {name}: {} {}",
        created.status,
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair_token: {}", created.body))
        .to_string();
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
    assert_eq!(
        paired.status,
        StatusCode::OK,
        "pair {name}: {}",
        paired.body
    );
    let host_id = paired.body["host_id"]
        .as_str()
        .unwrap_or_else(|| panic!("host_id: {}", paired.body))
        .to_string();
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
            "current_jobs": 0,
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat {name}: {}", hb.body);
    Uuid::parse_str(&host_id).expect("host uuid")
}

async fn assert_no_chat_turn(state: &AppState, workspace_id: Uuid, label: &str) {
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
        "{label} must not enqueue a chat_turn: {claimed:?}"
    );
}

async fn post_chat_complete_timed(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    body: Value,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, "POST", &chat_complete_path(ws), Some(jar), Some(body)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "POST /chat hung for more than 5s — foreign conversation_id must 404 before runner wait"
        )
    })
}

/// GET/DELETE /conversations/{id} and POST /chat complete cannot resume
/// another workspace's or a teammate's conversation_id. Membership 403
/// (non-member cookie) on by-id stays on
/// `chat_status_create_list_delete_other_user_forbidden`. List/create
/// other-ws 403 stays on
/// `chat_conversations_list_create_other_ws_are_403_and_do_not_insert`.
/// Stream stays on `-- stream_conversation`. Filter `-- conversation_user_workspace`.
#[tokio::test]
async fn chat_conversation_user_workspace_scope() {
    let (app, state) = app_with_state().await;
    let tag = Uuid::new_v4();
    let a_title = format!("unique-secret-{tag}");
    let unique_hi = format!("unique-hi-{tag}");
    let stored_history = format!("stored-history-{tag}");
    let secrets = [
        a_title.as_str(),
        unique_hi.as_str(),
        stored_history.as_str(),
    ];

    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");

    let created = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "title": a_title })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "POST /conversations: {}",
        created.body
    );
    let a_cid = created.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created.body))
        .to_string();
    let a_cid_uuid = Uuid::parse_str(&a_cid).expect("a_cid");

    // Plant stored history so a GET/complete that loads list_messages(cid)
    // without user+workspace is a visible leak. HTTP plant only.
    fleet_db::repos::chats::insert_message(
        &state.db,
        Uuid::now_v7(),
        a_cid_uuid,
        "user",
        &stored_history,
        None,
        None,
        &json!([]),
        chrono::Utc::now(),
    )
    .await
    .expect("plant stored user turn on A's thread");

    let snap = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "owner GET /conversations/{{id}}: {}",
        snap.body
    );
    assert_eq!(
        snap.body["conversation"]["title"].as_str(),
        Some(a_title.as_str()),
        "snapshot title: {}",
        snap.body
    );
    let snap_count = conversation_message_count(&snap.body);
    assert!(
        snap_count >= 1,
        "snapshot must include planted stored history: {}",
        snap.body
    );
    assert!(
        conversation_message_contents(&snap.body)
            .iter()
            .any(|c| c == &stored_history),
        "snapshot must include planted stored history: {}",
        snap.body
    );

    let listed_a = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        listed_a.status,
        StatusCode::OK,
        "owner list A: {}",
        listed_a.body
    );
    let a_convs = listed_a.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", listed_a.body));
    assert!(
        a_convs
            .iter()
            .any(|c| c["id"].as_str() == Some(a_cid.as_str())),
        "owner list A must include {a_cid}: {}",
        listed_a.body
    );

    // Same user, second workspace B. Pair a runner so skipping
    // ensure_conversation would enqueue chat_turn instead of 400.
    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&a_jar),
        Some(json!({ "name": format!("chat-scope-b-{tag}") })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "POST /api/workspaces B: {}",
        b_created.body
    );
    let b_ws = b_created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.id: {}", b_created.body))
        .to_string();
    assert_ne!(a_ws, b_ws, "A and B must be distinct workspaces");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");
    let b_runner = create_paired_online_runner(&app, &a_jar, &b_ws).await;

    let other_get = call(
        &app,
        "GET",
        &chat_conversation_path(&b_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &other_get,
        &secrets,
        "same user GET /workspaces/{B}/chat/conversations/{A_cid}",
    );

    let listed_b = call(
        &app,
        "GET",
        &chat_conversations_path(&b_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        listed_b.status,
        StatusCode::OK,
        "same user list B: {}",
        listed_b.body
    );
    let b_convs = listed_b.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", listed_b.body));
    assert!(
        b_convs
            .iter()
            .all(|c| c["id"].as_str() != Some(a_cid.as_str())),
        "B list must not include A's conversation {a_cid}: {}",
        listed_b.body
    );

    let other_complete = post_chat_complete_timed(
        &app,
        &a_jar,
        &b_ws,
        json!({
            "messages": [{ "role": "user", "content": unique_hi }],
            "provider": "grok",
            "conversation_id": a_cid,
            "runner_id": b_runner,
        }),
    )
    .await;
    assert_conversation_not_found(
        &other_complete,
        &secrets,
        "same user POST /workspaces/{B}/chat conversation_id=A",
    );
    // openai is not runner_cli — complete must 404 before "not configured".
    let other_openai = post_chat_complete_timed(
        &app,
        &a_jar,
        &b_ws,
        json!({
            "messages": [{ "role": "user", "content": unique_hi }],
            "provider": "openai",
            "conversation_id": a_cid,
        }),
    )
    .await;
    assert_conversation_not_found(
        &other_openai,
        &secrets,
        "same user POST /workspaces/{B}/chat openai conversation_id=A",
    );

    let after_b = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after_b,
        &a_cid,
        &a_title,
        snap_count,
        &stored_history,
        &unique_hi,
        "after B complete",
    );
    assert_no_chat_turn(&state, a_uuid, "B complete vs A's workspace").await;
    assert_no_chat_turn(&state, b_uuid, "B complete vs B's workspace").await;

    let other_delete = call(
        &app,
        "DELETE",
        &chat_conversation_path(&b_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &other_delete,
        &secrets,
        "same user DELETE /workspaces/{B}/chat/conversations/{A_cid}",
    );
    let after_b_delete = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after_b_delete,
        &a_cid,
        &a_title,
        snap_count,
        &stored_history,
        &unique_hi,
        "after B DELETE",
    );

    // Teammate on A who did not create the thread (member, so not 403).
    let (member_email, member_jar, _member_ws) = signup_and_login(&app).await;
    let invited = call(
        &app,
        "POST",
        &invites_path(&a_ws),
        Some(&a_jar),
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
        invited.body["added_existing_user"], true,
        "existing user must join immediately: {}",
        invited.body
    );

    let teammate_get = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&member_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &teammate_get,
        &secrets,
        "teammate GET /workspaces/{A}/chat/conversations/{owner_cid}",
    );

    let teammate_list = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(
        teammate_list.status,
        StatusCode::OK,
        "teammate list A: {}",
        teammate_list.body
    );
    let teammate_convs = teammate_list.body["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("conversations: {}", teammate_list.body));
    assert!(
        teammate_convs
            .iter()
            .all(|c| c["id"].as_str() != Some(a_cid.as_str())),
        "teammate list must not include owner's conversation {a_cid}: {}",
        teammate_list.body
    );

    let a_runner = create_paired_online_runner(&app, &a_jar, &a_ws).await;
    let teammate_complete = post_chat_complete_timed(
        &app,
        &member_jar,
        &a_ws,
        json!({
            "messages": [{ "role": "user", "content": unique_hi }],
            "provider": "grok",
            "conversation_id": a_cid,
            "runner_id": a_runner,
        }),
    )
    .await;
    assert_conversation_not_found(
        &teammate_complete,
        &secrets,
        "teammate POST /workspaces/{A}/chat conversation_id=owner",
    );
    let teammate_openai = post_chat_complete_timed(
        &app,
        &member_jar,
        &a_ws,
        json!({
            "messages": [{ "role": "user", "content": unique_hi }],
            "provider": "openai",
            "conversation_id": a_cid,
        }),
    )
    .await;
    assert_conversation_not_found(
        &teammate_openai,
        &secrets,
        "teammate POST /workspaces/{A}/chat openai conversation_id=owner",
    );
    assert_no_chat_turn(&state, a_uuid, "teammate complete vs A's workspace").await;

    let after_teammate_complete = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after_teammate_complete,
        &a_cid,
        &a_title,
        snap_count,
        &stored_history,
        &unique_hi,
        "after teammate complete",
    );

    let teammate_delete = call(
        &app,
        "DELETE",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&member_jar),
        None,
    )
    .await;
    assert_conversation_not_found(
        &teammate_delete,
        &secrets,
        "teammate DELETE /workspaces/{A}/chat/conversations/{owner_cid}",
    );

    let control_get = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &control_get,
        &a_cid,
        &a_title,
        snap_count,
        &stored_history,
        &unique_hi,
        "control owner GET",
    );
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
            runner_id: None,
            effort: None,
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
        run_list.iter().any(|r| r["id"].as_str() == Some(run_id)),
        "BFF runs missing tool run_id {run_id}: {}",
        runs.body
    );
}

/// `launch_agent_task` / `launch_agent_plan` spec must be ≤ 32_000 chars.
/// A 32001-char spec fails closed on `execute_tool` (ok:false, 400 spec
/// too long, no run_id) and inserts no accepted plan/run or board task.
/// Truncate-and-ok is a fail — silent truncate drops the tail of the
/// agent prompt and still looks applied. Distinct from the 200-char
/// title cap and the 1000-task cap. Exactly 32_000 still launches under
/// stub so this is not “every launch fails”. Keep
/// `chat_tool_launch_agent_task_builds_plan_and_settles_under_stub`.
#[tokio::test]
async fn launch_spec_over_32k_fails_closed_and_inserts_no_plan() {
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
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);

    let snap_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "snapshot tasks: {}",
        snap_tasks.body
    );
    let snap_task_ids = fleet_task_ids(&snap_tasks.body);
    let snap_task_titles = fleet_task_titles(&snap_tasks.body);

    fn tool_err(content: &Value, action: &fleet_server::chat_tools::ChatAction) -> String {
        content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str())
            .to_string()
    }

    fn spec_err_is_too_long(err: &str) {
        let lower = err.to_ascii_lowercase();
        assert!(
            !lower.contains("internal") && !lower.contains("500"),
            "over-32k spec must fail closed (400 spec too long), not 500: {err}"
        );
        assert!(
            lower.contains("spec")
                && (lower.contains("too long")
                    || lower.contains("32k")
                    || lower.contains("32000")
                    || lower.contains("32_000")),
            "400 must be spec too long (max 32k bytes), not the title/task cap: {err}"
        );
        assert!(
            !lower.contains("too many") && !lower.contains("1000"),
            "spec length is not the 1000-task cap: {err}"
        );
        assert!(
            !(lower.contains("title")
                && (lower.contains("too long") || lower.contains("200"))
                && !lower.contains("spec")),
            "spec length is not the 200-char title cap: {err}"
        );
    }

    fn assert_launch_spec_rejected(
        tool: &str,
        action: &fleet_server::chat_tools::ChatAction,
        content: &Value,
    ) {
        assert!(
            !action.ok,
            "{tool} 32001-char spec must fail closed (ok:false), not truncate-and-ok: {action:?} {content}"
        );
        assert_eq!(action.tool, tool, "{tool} action.tool: {action:?}");
        assert_eq!(
            action.run_id, None,
            "{tool} 32001-char spec must not return a run_id: {action:?}"
        );
        assert!(
            content.get("run_id").and_then(|v| v.as_str()).is_none(),
            "{tool} body must not include a run_id: {content}"
        );
        spec_err_is_too_long(&tool_err(content, action));
    }

    async fn assert_store_unchanged(
        app: &axum::Router,
        jar: &CookieJar,
        ws: &str,
        snap_ids: &[String],
        snap_task_ids: &[String],
        snap_task_titles: &[String],
        forbidden_titles: &[&str],
        label: &str,
    ) {
        let runs = call(app, "GET", &fleet(ws, "/runs"), Some(jar), None).await;
        assert_eq!(runs.status, StatusCode::OK, "{label} runs: {}", runs.body);
        let after_ids = fleet_run_ids(&runs.body);
        assert_eq!(
            after_ids, snap_ids,
            "{label} GET /fleet/runs must not grow an accepted plan/run: snap={snap_ids:?} after={after_ids:?} body={}",
            runs.body
        );

        let tasks = call(app, "GET", &fleet(ws, "/tasks"), Some(jar), None).await;
        assert_eq!(
            tasks.status,
            StatusCode::OK,
            "{label} tasks: {}",
            tasks.body
        );
        let after_task_ids = fleet_task_ids(&tasks.body);
        assert_eq!(
            after_task_ids, snap_task_ids,
            "{label} GET /fleet/tasks must not grow a board task: snap={snap_task_ids:?} after={after_task_ids:?} body={}",
            tasks.body
        );
        let after_titles = fleet_task_titles(&tasks.body);
        for title in forbidden_titles {
            if snap_task_titles.iter().any(|t| t == title) {
                continue;
            }
            assert!(
                !after_titles.iter().any(|t| t == title),
                "{label} GET /fleet/tasks must not include a new accepted plan titled {title:?}: {}",
                tasks.body
            );
        }
    }

    let over = "x".repeat(32_001);
    assert_eq!(over.len(), 32_001);
    let task_title = format!("launch-spec-task-over-{}", Uuid::new_v4());
    let plan_title = format!("launch-spec-plan-over-{}", Uuid::new_v4());

    let (task_content, task_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": task_title,
            "spec": over,
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert_launch_spec_rejected("launch_agent_task", &task_action, &task_content);
    assert_store_unchanged(
        &app,
        &jar,
        &ws,
        &snap_ids,
        &snap_task_ids,
        &snap_task_titles,
        &[task_title.as_str()],
        "launch_agent_task 32001-char spec",
    )
    .await;

    let (plan_content, plan_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [{
                "id": "t1",
                "title": plan_title,
                "spec": over,
                "worker_profile": "coding"
            }]
        }),
        &llm,
    )
    .await;
    assert_launch_spec_rejected("launch_agent_plan", &plan_action, &plan_content);
    assert_store_unchanged(
        &app,
        &jar,
        &ws,
        &snap_ids,
        &snap_task_ids,
        &snap_task_titles,
        &[task_title.as_str(), plan_title.as_str()],
        "launch_agent_plan 32001-char spec",
    )
    .await;

    let at_cap = "x".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);
    let cap_title = format!("launch-spec-ok-{}", Uuid::new_v4());
    let (cap_content, cap_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": cap_title,
            "spec": at_cap,
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        cap_action.ok,
        "exactly 32_000-char launch_agent_task spec must still succeed under stub: {cap_action:?} {cap_content}"
    );
    let cap_run = cap_action
        .run_id
        .as_deref()
        .unwrap_or_else(|| panic!("32_000-char launch must return run_id: {cap_action:?}"));
    assert!(
        Uuid::parse_str(cap_run).is_ok(),
        "32_000-char launch run_id must be a uuid: {cap_run:?} {cap_action:?}"
    );
    let after_cap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_cap.status,
        StatusCode::OK,
        "runs after 32_000-char launch: {}",
        after_cap.body
    );
    let after_cap_ids = fleet_run_ids(&after_cap.body);
    assert!(
        after_cap_ids.iter().any(|id| id == cap_run),
        "GET /fleet/runs must list 32_000-char-spec run {cap_run}: {}",
        after_cap.body
    );
    let after_cap_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        after_cap_tasks.status,
        StatusCode::OK,
        "tasks after 32_000-char launch: {}",
        after_cap_tasks.body
    );
    let after_cap_titles = fleet_task_titles(&after_cap_tasks.body);
    assert!(
        after_cap_titles.iter().any(|t| t == &cap_title),
        "GET /fleet/tasks must list 32_000-char-spec task {cap_title:?}: {}",
        after_cap_tasks.body
    );
}

/// `execute_tool` is bound to the HTTP session workspace. A member of A and B
/// who chats in A and passes `workspace_id=B` (or nested `args.workspace_id`)
/// must not list B's tasks, launch a plan on B, start a case on B, or query
/// B's integrations. Distinct from `chat_conversation_user_workspace_scope`
/// (`conversation_id` IDOR) and `chat_tools_team_chat_run_scoped_to_workspace`
/// (`run_id`). Filter `-- workspace_scope`.
#[tokio::test]
async fn chat_tools_payload_workspace_id_workspace_scope() {
    let (app, state) = app_with_state().await;
    let (_email, jar, a_ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let a_ws_id = Uuid::parse_str(&a_ws).expect("a_ws");

    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("tools-ws-b-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "POST /api/workspaces B: {}",
        b_created.body
    );
    let b_ws = b_created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.id: {}", b_created.body))
        .to_string();
    let b_ws_id = Uuid::parse_str(&b_ws).expect("b_ws");
    assert_ne!(a_ws_id, b_ws_id, "A and B must be distinct workspaces");

    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let a_title = format!("A-board-title-{}", Uuid::new_v4());
    let a_spec = format!("A-board-spec-{}", Uuid::new_v4());
    let b_title = format!("B-secret-title-{}", Uuid::new_v4());
    let b_spec = format!("B-secret-spec-{}", Uuid::new_v4());
    let b_case_title = format!("B-secret-case-{}", Uuid::new_v4());
    let stolen_launch = format!("must-not-land-on-B-launch-{}", Uuid::new_v4());
    let stolen_case = format!("must-not-land-on-B-case-{}", Uuid::new_v4());

    let (a_planted, a_plant) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": a_title,
            "spec": a_spec,
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        a_plant.ok,
        "plant A board via launch_agent_task: {a_plant:?} {a_planted}"
    );

    let (b_planted, b_plant) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": b_title,
            "spec": b_spec,
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        b_plant.ok,
        "plant B board via launch_agent_task: {b_plant:?} {b_planted}"
    );
    let b_case_id = create_open_case(&app, &jar, &b_ws, &b_case_title).await;

    let snap_tasks = call(&app, "GET", &fleet(&b_ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "B GET /fleet/tasks snapshot: {}",
        snap_tasks.body
    );
    let snap_task_ids = fleet_task_ids(&snap_tasks.body);
    let snap_task_titles = fleet_task_titles(&snap_tasks.body);
    assert!(
        snap_task_titles.iter().any(|t| t == &b_title),
        "B snapshot must include planted task {b_title} so the leak check is not vacuous: {}",
        snap_tasks.body
    );

    let snap_runs = call(&app, "GET", &fleet(&b_ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        snap_runs.status,
        StatusCode::OK,
        "B GET /fleet/runs snapshot: {}",
        snap_runs.body
    );
    let snap_run_ids = fleet_run_ids(&snap_runs.body);
    assert!(
        !snap_run_ids.is_empty(),
        "B snapshot must include the planted run: {}",
        snap_runs.body
    );

    let snap_cases = call(&app, "GET", &cases_path(&b_ws, ""), Some(&jar), None).await;
    assert_eq!(
        snap_cases.status,
        StatusCode::OK,
        "B GET /cases snapshot: {}",
        snap_cases.body
    );
    let snap_case_rows = listed_case_id_titles(&snap_cases.body);
    let snap_case_count = snap_cases.body["count"]
        .as_u64()
        .unwrap_or_else(|| snap_case_rows.len() as u64);
    assert!(
        snap_case_rows
            .iter()
            .any(|(id, title)| id == &b_case_id && title == &b_case_title),
        "B snapshot must include planted case {b_case_id} / {b_case_title}: {}",
        snap_cases.body
    );

    let secrets = [b_title.as_str(), b_spec.as_str(), b_case_title.as_str()];

    fn assert_foreign_workspace_rejected(
        tool: &str,
        label: &str,
        action: &fleet_server::chat_tools::ChatAction,
        content: &Value,
        secrets: &[&str],
    ) {
        assert!(
            !action.ok,
            "{tool} {label} must be action.ok=false (or 403 workspace_id does not match this chat session), not succeed as B: {action:?} {content}"
        );
        assert_eq!(action.tool, tool, "{tool} {label} action.tool: {action:?}");
        assert_eq!(
            action.run_id, None,
            "{tool} {label} must not return a run_id: {action:?}"
        );
        assert!(
            content.get("run_id").and_then(|v| v.as_str()).is_none(),
            "{tool} {label} body must not include a run_id: {content}"
        );
        if content.get("id").and_then(|v| v.as_str()).is_some() && content.get("title").is_some() {
            panic!("{tool} {label} must not return a case/plan body: {content}");
        }
        let err = content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str());
        let lower = err.to_ascii_lowercase();
        assert!(
            !lower.contains("internal server")
                && !lower.contains("database error")
                && !lower.contains("database unavailable")
                && !err.contains("500"),
            "{tool} {label} must not 500: {err} {content}"
        );
        assert!(
            lower.contains("workspace_id does not match this chat session")
                || (err.contains("403") && lower.contains("workspace")),
            "{tool} {label} must be 403 workspace_id does not match this chat session: {err} {content}"
        );
        let dumped = format!("{action:?}{content}");
        for secret in secrets {
            assert!(
                !dumped.contains(secret),
                "{tool} {label} body / action must not include B's {secret:?}: {dumped}"
            );
        }
    }

    let foreign_calls: Vec<(&str, Value)> = vec![
        ("list_tasks", json!({ "workspace_id": b_ws })),
        ("list_tasks", json!({ "args": { "workspace_id": b_ws } })),
        ("get_fleet_status", json!({ "workspace_id": b_ws })),
        (
            "get_fleet_status",
            json!({ "args": { "workspace_id": b_ws } }),
        ),
        (
            "launch_agent_task",
            json!({
                "title": stolen_launch,
                "spec": "must not accept a plan on B",
                "worker_profile": "coding",
                "workspace_id": b_ws
            }),
        ),
        (
            "launch_agent_task",
            json!({
                "title": stolen_launch,
                "spec": "must not accept a plan on B",
                "worker_profile": "coding",
                "args": { "workspace_id": b_ws }
            }),
        ),
        (
            "start_case",
            json!({
                "title": stolen_case,
                "policy_md": "must not start on B",
                "workspace_id": b_ws
            }),
        ),
        (
            "start_case",
            json!({
                "title": stolen_case,
                "policy_md": "must not start on B",
                "args": { "workspace_id": b_ws }
            }),
        ),
        (
            "integration_query",
            json!({
                "provider": "slack",
                "operation": "list_channels",
                "workspace_id": b_ws
            }),
        ),
        (
            "integration_query",
            json!({
                "provider": "slack",
                "operation": "list_channels",
                "args": { "workspace_id": b_ws }
            }),
        ),
    ];

    for (tool, args) in &foreign_calls {
        let label = if args
            .get("args")
            .and_then(|a| a.get("workspace_id"))
            .is_some()
        {
            "nested args.workspace_id=B"
        } else {
            "workspace_id=B"
        };
        let (content, action) =
            fleet_server::chat_tools::execute_tool(&state, a_ws_id, user_id, tool, args, &llm)
                .await;
        assert_foreign_workspace_rejected(tool, label, &action, &content, &secrets);
    }

    let after_tasks = call(&app, "GET", &fleet(&b_ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        after_tasks.status,
        StatusCode::OK,
        "B GET /fleet/tasks after foreign tools: {}",
        after_tasks.body
    );
    assert_eq!(
        fleet_task_ids(&after_tasks.body),
        snap_task_ids,
        "B GET /fleet/tasks must still be the snapshot (no new accepted plan): snap={snap_task_ids:?} after={:?} body={}",
        fleet_task_ids(&after_tasks.body),
        after_tasks.body
    );
    assert_eq!(
        fleet_task_titles(&after_tasks.body),
        snap_task_titles,
        "B GET /fleet/tasks titles must still be the snapshot: {}",
        after_tasks.body
    );
    assert!(
        !fleet_task_titles(&after_tasks.body)
            .iter()
            .any(|t| t == &stolen_launch),
        "B must not grow a task titled {stolen_launch}: {}",
        after_tasks.body
    );

    let after_runs = call(&app, "GET", &fleet(&b_ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_runs.status,
        StatusCode::OK,
        "B GET /fleet/runs after foreign tools: {}",
        after_runs.body
    );
    assert_eq!(
        fleet_run_ids(&after_runs.body),
        snap_run_ids,
        "B GET /fleet/runs must still be the snapshot (no new accepted plan): snap={snap_run_ids:?} after={:?} body={}",
        fleet_run_ids(&after_runs.body),
        after_runs.body
    );

    let after_cases = call(&app, "GET", &cases_path(&b_ws, ""), Some(&jar), None).await;
    assert_eq!(
        after_cases.status,
        StatusCode::OK,
        "B GET /cases after foreign tools: {}",
        after_cases.body
    );
    let after_case_rows = listed_case_id_titles(&after_cases.body);
    let after_case_count = after_cases.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_case_rows.len() as u64);
    assert_eq!(
        after_case_count, snap_case_count,
        "B GET /cases count must still be the snapshot (no new case): snap={snap_case_count} after={after_case_count} body={}",
        after_cases.body
    );
    assert_eq!(
        after_case_rows, snap_case_rows,
        "B GET /cases must still be the snapshot: snap={snap_case_rows:?} after={after_case_rows:?} body={}",
        after_cases.body
    );
    assert!(
        after_case_rows.iter().all(|(_, t)| t != &stolen_case),
        "B must not grow a case titled {stolen_case}: {}",
        after_cases.body
    );

    let a_tasks_before_ok = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        a_tasks_before_ok.status,
        StatusCode::OK,
        "A GET /fleet/tasks after rejects: {}",
        a_tasks_before_ok.body
    );
    assert!(
        !fleet_task_titles(&a_tasks_before_ok.body)
            .iter()
            .any(|t| t == &stolen_launch),
        "rejected launch_agent_task must not insert on A either: {}",
        a_tasks_before_ok.body
    );
    let a_cases_before_ok = call(&app, "GET", &cases_path(&a_ws, ""), Some(&jar), None).await;
    assert_eq!(
        a_cases_before_ok.status,
        StatusCode::OK,
        "A GET /cases after rejects: {}",
        a_cases_before_ok.body
    );
    assert!(
        listed_case_id_titles(&a_cases_before_ok.body)
            .iter()
            .all(|(_, t)| t != &stolen_case),
        "rejected start_case must not insert on A either: {}",
        a_cases_before_ok.body
    );

    let (listed_a, list_a) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "list_tasks",
        &json!({ "workspace_id": a_ws }),
        &llm,
    )
    .await;
    assert!(
        list_a.ok,
        "list_tasks with workspace_id=A (the session) may proceed: {list_a:?} {listed_a}"
    );
    let listed_a_s = format!("{list_a:?}{listed_a}");
    assert!(
        listed_a_s.contains(&a_title),
        "list_tasks workspace_id=A must list A's board ({a_title}): {listed_a}"
    );
    assert!(
        !listed_a_s.contains(&b_title) && !listed_a_s.contains(&b_spec),
        "list_tasks workspace_id=A must not list B's titles/spec_md: {listed_a}"
    );

    let (status_a, status_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "get_fleet_status",
        &json!({ "workspace_id": a_ws }),
        &llm,
    )
    .await;
    assert!(
        status_action.ok,
        "get_fleet_status with workspace_id=A may proceed: {status_action:?} {status_a}"
    );
    let status_s = format!("{status_action:?}{status_a}");
    assert!(
        !status_s.contains(&b_title) && !status_s.contains(&b_spec),
        "get_fleet_status workspace_id=A must not include B's titles/spec_md: {status_a}"
    );

    let a_ok_launch = format!("A-ok-launch-{}", Uuid::new_v4());
    let a_ok_spec = format!("A-ok-spec-{}", Uuid::new_v4());
    let (launched, launch_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": a_ok_launch,
            "spec": a_ok_spec,
            "worker_profile": "coding",
            "workspace_id": a_ws
        }),
        &llm,
    )
    .await;
    assert!(
        launch_action.ok,
        "launch_agent_task with workspace_id=A may insert on A: {launch_action:?} {launched}"
    );
    let a_ok_run = launch_action
        .run_id
        .as_deref()
        .unwrap_or_else(|| panic!("A-side launch must return run_id: {launch_action:?}"));

    let a_ok_case = format!("A-ok-case-{}", Uuid::new_v4());
    let (started, start_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "start_case",
        &json!({
            "title": a_ok_case,
            "policy_md": "session workspace A",
            "workspace_id": a_ws
        }),
        &llm,
    )
    .await;
    assert!(
        start_action.ok,
        "start_case with workspace_id=A may insert on A: {start_action:?} {started}"
    );

    let (iq_a, iq_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "integration_query",
        &json!({
            "provider": "slack",
            "operation": "list_channels",
            "workspace_id": a_ws,
            "args": { "workspace_id": a_ws }
        }),
        &llm,
    )
    .await;
    let iq_err = iq_a
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or(iq_action.summary.as_str());
    assert!(
        !iq_err
            .to_ascii_lowercase()
            .contains("workspace_id does not match this chat session"),
        "integration_query with workspace_id=A may proceed (must not 403 session mismatch): {iq_action:?} {iq_a}"
    );

    let a_tasks_ok = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        a_tasks_ok.status,
        StatusCode::OK,
        "A GET /fleet/tasks after session-A tools: {}",
        a_tasks_ok.body
    );
    let a_ok_titles = fleet_task_titles(&a_tasks_ok.body);
    assert!(
        a_ok_titles.iter().any(|t| t == &a_title) && a_ok_titles.iter().any(|t| t == &a_ok_launch),
        "A GET /fleet/tasks must include planted + session-A launch titles: {}",
        a_tasks_ok.body
    );
    assert!(
        a_ok_titles
            .iter()
            .all(|t| t != &b_title && t != &stolen_launch),
        "A GET /fleet/tasks must not include B's title or the rejected launch: {}",
        a_tasks_ok.body
    );
    let a_runs_ok = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        a_runs_ok.status,
        StatusCode::OK,
        "A GET /fleet/runs after session-A launch: {}",
        a_runs_ok.body
    );
    assert!(
        fleet_run_ids(&a_runs_ok.body)
            .iter()
            .any(|id| id == a_ok_run),
        "A GET /fleet/runs must list session-A launch {a_ok_run}: {}",
        a_runs_ok.body
    );
    let a_cases_ok = call(&app, "GET", &cases_path(&a_ws, ""), Some(&jar), None).await;
    assert_eq!(
        a_cases_ok.status,
        StatusCode::OK,
        "A GET /cases after session-A start_case: {}",
        a_cases_ok.body
    );
    assert!(
        listed_case_id_titles(&a_cases_ok.body)
            .iter()
            .any(|(_, t)| t == &a_ok_case),
        "A GET /cases must include session-A start_case {a_ok_case}: {}",
        a_cases_ok.body
    );

    let b_tasks_final = call(&app, "GET", &fleet(&b_ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        b_tasks_final.status,
        StatusCode::OK,
        "B GET /fleet/tasks after session-A inserts: {}",
        b_tasks_final.body
    );
    assert_eq!(
        fleet_task_ids(&b_tasks_final.body),
        snap_task_ids,
        "session-A launch/start_case must insert on A only — B tasks still the snapshot: {}",
        b_tasks_final.body
    );
    let b_runs_final = call(&app, "GET", &fleet(&b_ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        b_runs_final.status,
        StatusCode::OK,
        "B GET /fleet/runs after session-A inserts: {}",
        b_runs_final.body
    );
    assert_eq!(
        fleet_run_ids(&b_runs_final.body),
        snap_run_ids,
        "session-A launch must insert on A only — B runs still the snapshot: {}",
        b_runs_final.body
    );
    let b_cases_final = call(&app, "GET", &cases_path(&b_ws, ""), Some(&jar), None).await;
    assert_eq!(
        b_cases_final.status,
        StatusCode::OK,
        "B GET /cases after session-A inserts: {}",
        b_cases_final.body
    );
    assert_eq!(
        listed_case_id_titles(&b_cases_final.body),
        snap_case_rows,
        "session-A start_case must insert on A only — B cases still the snapshot: {}",
        b_cases_final.body
    );
    let b_final_s = format!(
        "{}{}{}",
        b_tasks_final.body, b_runs_final.body, b_cases_final.body
    );
    assert!(
        !b_final_s.contains(&a_ok_launch)
            && !b_final_s.contains(&a_ok_spec)
            && !b_final_s.contains(&a_ok_case)
            && !b_final_s.contains(a_ok_run),
        "B must not include A's session-A launch/case: tasks={} runs={} cases={}",
        b_tasks_final.body,
        b_runs_final.body,
        b_cases_final.body
    );

    let (listed_omit, list_omit) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "list_tasks",
        &json!({}),
        &llm,
    )
    .await;
    assert!(
        list_omit.ok,
        "control: list_tasks without workspace_id must still list A's board so this is not a tautology that every tool 403s: {list_omit:?} {listed_omit}"
    );
    let omit_s = format!("{list_omit:?}{listed_omit}");
    assert!(
        omit_s.contains(&a_title) && omit_s.contains(&a_ok_launch),
        "control: omitted workspace_id must list A's board: {listed_omit}"
    );
    assert!(
        !omit_s.contains(&b_title) && !omit_s.contains(&b_spec) && !omit_s.contains(&b_case_title),
        "control: omitted workspace_id must not list B: {listed_omit}"
    );
}

/// Docs §7.2: `get_integration` / `list_integrations` after a connected row
/// must not put tokens or the credentials envelope into content / detail /
/// summary / the raw tool JSON (those persist into chat history). Distinct
/// from GET /integrations HTTP omit cards, the at-rest envelope card, and
/// `integration_query` sanitize. Product already omits — this is the lock.
#[tokio::test]
async fn get_integration_omits_secrets() {
    use fleet_db::repos::integrations::{self as integ_repo, UpsertConnection};

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
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let tag = Uuid::now_v7();
    let access = format!("tok_get_int_omits_access_{tag}");
    let refresh = format!("tok_get_int_omits_refresh_{tag}");
    let client_secret = format!("tok_get_int_omits_client_secret_{tag}");
    let plain_access = format!("tok_get_int_omits_plain_access_{tag}");
    let plain_refresh = format!("tok_get_int_omits_plain_refresh_{tag}");
    let unique_secrets = [
        access.as_str(),
        refresh.as_str(),
        client_secret.as_str(),
        plain_access.as_str(),
        plain_refresh.as_str(),
    ];

    let sealed = state
        .integrations
        .credentials_key
        .encrypt(&json!({
            "access_token": access,
            "refresh_token": refresh,
            "client_secret": client_secret,
        }))
        .expect("seal trello credentials");
    assert_eq!(
        sealed.get("_enc").and_then(|v| v.as_str()),
        Some("v1"),
        "seed must be an encrypt() envelope: {sealed}"
    );
    let env_nonce = sealed["nonce"]
        .as_str()
        .unwrap_or_else(|| panic!("envelope nonce: {sealed}"))
        .to_string();
    let env_ct = sealed["ct"]
        .as_str()
        .unwrap_or_else(|| panic!("envelope ct: {sealed}"))
        .to_string();
    assert!(
        !env_nonce.is_empty() && !env_ct.is_empty(),
        "envelope nonce/ct must be unique material to lock against: {sealed}"
    );

    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: "trello".into(),
            account_label: Some(format!("trello-omits-{tag}")),
            config: json!({ "site": "trello.com" }),
            credentials: sealed.clone(),
            connected_by: Some(user_id),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("upsert connected trello");

    let stored = credentials_for(
        &state.db,
        workspace_id,
        "trello",
        &state.integrations.credentials_key,
    )
    .await
    .expect("credentials_for after seed")
    .expect("connected trello must store credentials (not a tautology that nothing was stored)");
    assert_eq!(stored["access_token"], access);
    assert_eq!(stored["refresh_token"], refresh);
    assert_eq!(stored["client_secret"], client_secret);
    let row = integ_repo::find_connected(&state.db, workspace_id, "trello")
        .await
        .expect("find_connected after seed")
        .expect("trello row");
    assert_eq!(row.credentials["_enc"], "v1");
    assert_eq!(row.credentials["nonce"], env_nonce);
    assert_eq!(row.credentials["ct"], env_ct);

    fn json_has_key(v: &Value, key: &str) -> bool {
        match v {
            Value::Object(map) => {
                map.contains_key(key) || map.values().any(|child| json_has_key(child, key))
            }
            Value::Array(arr) => arr.iter().any(|child| json_has_key(child, key)),
            _ => false,
        }
    }

    fn assert_omits_secrets(
        label: &str,
        content: &Value,
        action: &fleet_server::chat_tools::ChatAction,
        unique: &[&str],
        envelope_nonce: &str,
        envelope_ct: &str,
    ) {
        let content_s = serde_json::to_string(content).expect("serialize content");
        let action_s = serde_json::to_string(action).expect("serialize action");
        let detail_s = action
            .detail
            .as_ref()
            .map(|d| serde_json::to_string(d).expect("serialize detail"))
            .unwrap_or_default();
        let dump = format!(
            "{content_s}\n{action_s}\n{}\n{detail_s}\n{action:?}",
            action.summary
        );
        for needle in unique {
            assert!(
                !content_s.contains(needle),
                "{label} content must not contain unique secret {needle}: {content_s}"
            );
            assert!(
                !action.summary.contains(needle),
                "{label} summary must not contain unique secret {needle}: {}",
                action.summary
            );
            assert!(
                !detail_s.contains(needle),
                "{label} detail must not contain unique secret {needle}: {detail_s}"
            );
            assert!(
                !dump.contains(needle),
                "{label} raw tool JSON must not contain unique secret {needle}: {dump}"
            );
        }
        for key in [
            "credentials",
            "access_token",
            "refresh_token",
            "client_secret",
            "_enc",
            "nonce",
            "ct",
        ] {
            assert!(
                !json_has_key(content, key),
                "{label} content must not have a {key} key: {content}"
            );
            if let Some(detail) = action.detail.as_ref() {
                assert!(
                    !json_has_key(detail, key),
                    "{label} detail must not have a {key} key: {detail}"
                );
            }
        }
        for needle in ["_enc", envelope_nonce, envelope_ct] {
            assert!(
                !dump.contains(needle),
                "{label} must not echo envelope material {needle}: {dump}"
            );
        }
        assert!(
            !dump.contains("\"nonce\"") && !dump.contains("\"ct\""),
            "{label} must not contain envelope keys nonce/ct: {dump}"
        );
    }

    async fn tool(
        state: &AppState,
        workspace_id: Uuid,
        user_id: Uuid,
        llm: &fleet_server::chat_tools::ChatLlmContext,
        name: &str,
        arguments: Value,
    ) -> (Value, fleet_server::chat_tools::ChatAction) {
        fleet_server::chat_tools::execute_tool(state, workspace_id, user_id, name, &arguments, llm)
            .await
    }

    let (got, got_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "get_integration",
        json!({ "provider": "trello" }),
    )
    .await;
    assert!(
        got_action.ok,
        "get_integration trello must be action.ok=true after seed: action={got_action:?} content={got}"
    );
    assert_eq!(got_action.tool, "get_integration");
    assert_eq!(
        got["connected"], true,
        "get_integration trello must be connected after seed: {got}"
    );
    assert_eq!(got["id"], "trello", "{got}");
    assert_omits_secrets(
        "get_integration trello",
        &got,
        &got_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );

    let (listed, list_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "list_integrations",
        json!({}),
    )
    .await;
    assert!(
        list_action.ok,
        "list_integrations {{}}: action={list_action:?} content={listed}"
    );
    assert_eq!(list_action.tool, "list_integrations");
    let trello_listed = listed["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == "trello")
        .unwrap_or_else(|| panic!("trello catalog row: {listed}"));
    assert_eq!(
        trello_listed["connected"], true,
        "list_integrations trello connected flag: {trello_listed}"
    );
    assert_omits_secrets(
        "list_integrations {{}}",
        &listed,
        &list_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );

    let (only, only_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "list_integrations",
        json!({ "connected_only": true }),
    )
    .await;
    assert!(
        only_action.ok,
        "list_integrations connected_only: action={only_action:?} content={only}"
    );
    assert_eq!(only_action.tool, "list_integrations");
    let only_items = only["integrations"]
        .as_array()
        .unwrap_or_else(|| panic!("connected_only integrations array: {only}"));
    assert!(
        only_items
            .iter()
            .any(|i| i["id"] == "trello" && i["connected"] == true),
        "connected_only must include connected trello: {only}"
    );
    assert!(
        only_items.iter().all(|i| i["connected"] == true),
        "connected_only must not dump disconnected catalog rows: {only}"
    );
    assert_omits_secrets(
        "list_integrations connected_only",
        &only,
        &only_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );

    let catalog = call(&app, "GET", &integrations_path(&ws), Some(&jar), None).await;
    assert_eq!(
        catalog.status,
        StatusCode::OK,
        "GET /integrations after seed: {}",
        catalog.body
    );
    assert_eq!(
        catalog_row(&catalog.body, "trello")["connected"],
        true,
        "GET /integrations trello must stay connected: {}",
        catalog.body
    );
    let catalog_obj = catalog
        .body
        .as_object()
        .unwrap_or_else(|| panic!("GET /integrations object: {}", catalog.body));
    assert!(
        !catalog_obj.contains_key("credentials"),
        "GET /integrations must not have a credentials key: {}",
        catalog.body
    );
    let catalog_hay = format!("{} {}", catalog.raw, catalog.body);
    for needle in unique_secrets {
        assert!(
            !catalog_hay.contains(needle),
            "GET /integrations must not echo unique secret {needle}: {}",
            catalog.body
        );
    }
    for needle in ["access_token", "refresh_token", "client_secret", "_enc"] {
        assert!(
            !catalog_hay.contains(needle),
            "GET /integrations must not contain {needle}: {}",
            catalog.body
        );
    }
    assert!(
        !catalog_hay.contains(&env_nonce) && !catalog_hay.contains(&env_ct),
        "GET /integrations must not echo the sealed envelope: {}",
        catalog.body
    );

    let (unknown, unknown_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "get_integration",
        json!({ "provider": "not_a_real_provider" }),
    )
    .await;
    assert!(
        !unknown_action.ok,
        "unknown provider must be action.ok=false (400), not a 200 that returns secrets: action={unknown_action:?} content={unknown}"
    );
    assert_eq!(unknown_action.tool, "get_integration");
    let unknown_err = format!(
        "{} {}",
        unknown["error"].as_str().unwrap_or(""),
        unknown_action.summary
    )
    .to_ascii_lowercase();
    assert!(
        unknown_err.contains("unknown provider"),
        "unknown provider must say unknown provider (AppError::bad_request / 400), not dump the row: {unknown} {unknown_action:?}"
    );
    assert!(
        unknown_err.contains("not_a_real_provider"),
        "unknown provider error must name the id: {unknown}"
    );
    assert_ne!(
        unknown.get("connected"),
        Some(&json!(true)),
        "unknown provider must not be a 200-shaped connected payload: {unknown}"
    );
    assert_omits_secrets(
        "get_integration unknown provider",
        &unknown,
        &unknown_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );

    // Legacy plaintext blob is also a lock — encrypt() is preferred, but a
    // handler that Json(conn) must not echo either storage shape.
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: "github".into(),
            account_label: Some(format!("github-plain-{tag}")),
            config: json!({ "site": "github.com" }),
            credentials: json!({
                "access_token": plain_access,
                "refresh_token": plain_refresh,
            }),
            connected_by: Some(user_id),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("upsert github plaintext credentials");
    let stored_plain = credentials_for(
        &state.db,
        workspace_id,
        "github",
        &state.integrations.credentials_key,
    )
    .await
    .expect("credentials_for plaintext github")
    .expect("legacy plaintext blob must still load");
    assert_eq!(stored_plain["access_token"], plain_access);
    let (gh, gh_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "get_integration",
        json!({ "provider": "github" }),
    )
    .await;
    assert!(
        gh_action.ok,
        "get_integration github plaintext row: action={gh_action:?} content={gh}"
    );
    assert_eq!(gh["connected"], true, "{gh}");
    assert_omits_secrets(
        "get_integration github plaintext",
        &gh,
        &gh_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );
    let (listed_plain, listed_plain_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "list_integrations",
        json!({ "connected_only": true }),
    )
    .await;
    assert!(
        listed_plain_action.ok,
        "list_integrations after plaintext seed: {listed_plain_action:?} {listed_plain}"
    );
    assert_omits_secrets(
        "list_integrations after plaintext seed",
        &listed_plain,
        &listed_plain_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );

    let disc = call(
        &app,
        "DELETE",
        &format!("{}/trello", integrations_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        disc.status,
        StatusCode::OK,
        "DELETE connected trello: {}",
        disc.body
    );
    let (after, after_action) = tool(
        &state,
        workspace_id,
        user_id,
        &llm,
        "get_integration",
        json!({ "provider": "trello" }),
    )
    .await;
    assert!(
        after_action.ok,
        "get_integration trello after DELETE must still be ok (connected false), not 500: action={after_action:?} content={after}"
    );
    assert_eq!(
        after["connected"], false,
        "get_integration trello after DELETE must be connected false: {after}"
    );
    assert_omits_secrets(
        "get_integration trello after DELETE",
        &after,
        &after_action,
        &unique_secrets,
        &env_nonce,
        &env_ct,
    );
}

/// Docs §7.3: `execute_tool("integration_query")` with a supported provider
/// and a garbage operation is `ok:false` `unknown_op`. It must not treat
/// unknown as `list_repos`, persist a connection, return secrets, or open
/// github.com. Distinct from the decrypt/sanitize lock and from
/// list-github-commit-files-limits (size cap on a valid op).
#[tokio::test]
async fn integration_query_unknown_op_is_ok_false_and_does_not_call_upstream() {
    use fleet_db::repos::integrations::{self as integ_repo};

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

    let before = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .expect("list connections before");
    assert!(
        before.is_empty(),
        "fresh workspace must start with no integration rows: {before:?}"
    );
    for provider in ["github", "gitlab", "bitbucket"] {
        assert!(
            integ_repo::find_connected(&state.db, workspace_id, provider)
                .await
                .unwrap_or_else(|e| panic!("find_connected {provider} empty: {e}"))
                .is_none(),
            "workspace must start with no {provider} row"
        );
    }

    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    fn dump_payload(content: &Value, action: &fleet_server::chat_tools::ChatAction) -> String {
        format!(
            "{} {}",
            serde_json::to_string(content).expect("serialize content"),
            serde_json::to_string(action).expect("serialize action")
        )
    }

    fn assert_no_secrets_or_upstream(label: &str, dump: &str) {
        for needle in ["access_token", "refresh_token", "client_secret"] {
            assert!(
                !dump.contains(needle),
                "{label} must not contain {needle}: {dump}"
            );
        }
        for needle in [
            "github.com",
            "api.github.com",
            "gitlab.com",
            "bitbucket.org",
            "api.bitbucket.org",
        ] {
            assert!(
                !dump.contains(needle),
                "{label} must not open upstream ({needle}): {dump}"
            );
        }
        assert!(
            !dump.contains("\"500\"") && !dump.to_ascii_lowercase().contains("internal server"),
            "{label} must not 500: {dump}"
        );
    }

    async fn query(
        state: &AppState,
        workspace_id: Uuid,
        user_id: Uuid,
        llm: &fleet_server::chat_tools::ChatLlmContext,
        provider: &str,
        operation: &str,
    ) -> (Value, fleet_server::chat_tools::ChatAction) {
        fleet_server::chat_tools::execute_tool(
            state,
            workspace_id,
            user_id,
            "integration_query",
            &json!({
                "provider": provider,
                "operation": operation,
                "args": {},
            }),
            llm,
        )
        .await
    }

    async fn assert_unknown_op(
        state: &AppState,
        workspace_id: Uuid,
        user_id: Uuid,
        llm: &fleet_server::chat_tools::ChatLlmContext,
        provider: &str,
        operation: &str,
    ) {
        let (content, action) = query(state, workspace_id, user_id, llm, provider, operation).await;
        assert!(
            !action.ok,
            "{provider}/{operation} must be action.ok=false (not 500 / not forwarded): action={action:?} content={content}"
        );
        assert_eq!(action.tool, "integration_query");
        assert_eq!(
            content["ok"], false,
            "{provider}/{operation} body must be ok:false: {content}"
        );
        let code = content["error_code"].as_str();
        let summary = format!(
            "{} {}",
            content["summary"].as_str().unwrap_or(""),
            action.summary
        )
        .to_ascii_lowercase();
        let unknown_in_message = summary.contains("unknown") && summary.contains("operation");
        assert!(
            code == Some("unknown_op") || unknown_in_message,
            "{provider}/{operation} must be error_code unknown_op (or message unknown+operation), not forwarded as list_repos / 500: {content}"
        );
        assert_ne!(
            code,
            Some("not_connected"),
            "{provider}/{operation} must be unknown_op before load — treating garbage as list_repos is the failure: {content}"
        );
        assert_ne!(
            code,
            Some("internal"),
            "{provider}/{operation} must not 500/internal: {content}"
        );
        assert!(
            content["data"].is_null(),
            "{provider}/{operation} data must be Null (no upstream payload): {content}"
        );
        assert!(
            content["provider_status"].is_null(),
            "{provider}/{operation} must not carry an upstream provider_status: {content}"
        );
        let dump = dump_payload(&content, &action);
        assert_no_secrets_or_upstream(&format!("{provider}/{operation}"), &dump);
    }

    for operation in ["delete_repo", "raw_http", "not_a_real_op"] {
        assert_unknown_op(&state, workspace_id, user_id, &llm, "github", operation).await;
    }
    assert_unknown_op(
        &state,
        workspace_id,
        user_id,
        &llm,
        "gitlab",
        "delete_project",
    )
    .await;

    let (bb_content, bb_action) = query(
        &state,
        workspace_id,
        user_id,
        &llm,
        "bitbucket",
        "create_pr",
    )
    .await;
    assert!(
        !bb_action.ok,
        "coming-soon bitbucket create_pr must be ok:false: action={bb_action:?} content={bb_content}"
    );
    assert_eq!(bb_action.tool, "integration_query");
    assert_eq!(
        bb_content["ok"], false,
        "bitbucket create_pr body must be ok:false: {bb_content}"
    );
    let bb_code = bb_content["error_code"].as_str();
    assert!(
        matches!(bb_code, Some("unknown_provider") | Some("not_connected")),
        "coming-soon bitbucket create_pr may be unknown_provider or not_connected (must not persist or return secrets): {bb_content}"
    );
    assert_ne!(
        bb_code,
        Some("internal"),
        "bitbucket create_pr must not 500/internal: {bb_content}"
    );
    assert!(
        bb_content["data"].is_null(),
        "bitbucket create_pr data must be Null: {bb_content}"
    );
    let bb_dump = dump_payload(&bb_content, &bb_action);
    assert_no_secrets_or_upstream("bitbucket/create_pr", &bb_dump);

    let (control_content, control_action) =
        query(&state, workspace_id, user_id, &llm, "github", "list_repos").await;
    assert!(
        !control_action.ok,
        "disconnected list_repos must be ok:false: action={control_action:?} content={control_content}"
    );
    assert_eq!(control_action.tool, "integration_query");
    assert_eq!(
        control_content["error_code"].as_str(),
        Some("not_connected"),
        "control list_repos while disconnected must be not_connected (not unknown_op — this is not a tautology that every query is unknown_op): {control_content}"
    );
    assert_ne!(
        control_content["error_code"].as_str(),
        Some("unknown_op"),
        "list_repos is allowlisted: {control_content}"
    );
    let control_dump = dump_payload(&control_content, &control_action);
    assert_no_secrets_or_upstream("github/list_repos disconnected", &control_dump);

    let after = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .expect("list connections after");
    assert_eq!(
        after.len(),
        before.len(),
        "unknown op / coming-soon query must not persist a connection: before={before:?} after={after:?}"
    );
    for provider in ["github", "gitlab", "bitbucket"] {
        assert!(
            integ_repo::find_connected(&state.db, workspace_id, provider)
                .await
                .unwrap_or_else(|e| panic!("find_connected {provider} after: {e}"))
                .is_none(),
            "{provider} must still have no connected row after unknown/coming-soon queries"
        );
    }

    let catalog = call(&app, "GET", &integrations_path(&ws), Some(&jar), None).await;
    assert_eq!(
        catalog.status,
        StatusCode::OK,
        "GET /integrations after unknown_op: {}",
        catalog.body
    );
    for provider in ["github", "gitlab", "bitbucket"] {
        assert_eq!(
            catalog_row(&catalog.body, provider)["connected"],
            false,
            "GET /integrations {provider} must stay disconnected: {}",
            catalog.body
        );
    }
    let catalog_hay = format!("{} {}", catalog.raw, catalog.body);
    for needle in ["access_token", "refresh_token", "client_secret"] {
        assert!(
            !catalog_hay.contains(needle),
            "GET /integrations after unknown_op must not contain {needle}: {}",
            catalog.body
        );
    }
}

/// Docs §7.3: `execute_tool("integration_query")` decrypts on the control
/// plane and never echoes the token. Live app — not a reconstructed
/// `tool_body` / `ChatAction`.
#[tokio::test]
async fn integration_query_decrypts_server_side_and_never_echoes_token() {
    use fleet_db::repos::integrations::{self as integ_repo, UpsertConnection};

    const LEAK: &str = "gho_IQ_CHAT_LIVE_SANITIZE_MUST_NOT_ECHO_99ab";

    fn assert_no_token_echo(label: &str, dump: &str) {
        assert!(
            !dump.contains("access_token")
                && !dump.contains("refresh_token")
                && !dump.contains(LEAK),
            "{label} must not contain access_token, refresh_token, or the sealed token: {dump}"
        );
    }

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
    assert!(
        integ_repo::find_connected(&state.db, workspace_id, "github")
            .await
            .expect("find_connected empty")
            .is_none(),
        "workspace must start with no github row"
    );

    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };
    let stuffed_args = json!({
        "access_token": LEAK,
        "refresh_token": LEAK,
        "token": LEAK,
        "owner": "acme",
        "repo": "app",
    });

    let (unknown_content, unknown_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "integration_query",
        &json!({
            "provider": "github",
            "operation": "raw_http",
            "args": stuffed_args,
        }),
        &llm,
    )
    .await;
    assert!(
        !unknown_action.ok,
        "unknown op must be ok:false: action={unknown_action:?} content={unknown_content}"
    );
    assert_eq!(unknown_action.tool, "integration_query");
    assert_eq!(
        unknown_content["ok"], false,
        "unknown_op body must be ok:false: {unknown_content}"
    );
    assert_eq!(
        unknown_content["error_code"].as_str(),
        Some("unknown_op"),
        "docs §7.3 unknown op must be unknown_op: {unknown_content}"
    );
    assert!(
        unknown_content["data"].is_null(),
        "unknown_op data must be Null: {unknown_content}"
    );
    assert_no_token_echo(
        "unknown_op execute_tool Value",
        &serde_json::to_string(&unknown_content).expect("serialize unknown content"),
    );
    assert_no_token_echo(
        "unknown_op ChatAction",
        &serde_json::to_string(&unknown_action).expect("serialize unknown action"),
    );

    let (disconnected_content, disconnected_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "integration_query",
        &json!({
            "provider": "github",
            "operation": "list_repos",
            "args": stuffed_args,
        }),
        &llm,
    )
    .await;
    assert!(
        !disconnected_action.ok,
        "disconnected list_repos must be ok:false: action={disconnected_action:?} content={disconnected_content}"
    );
    assert_eq!(disconnected_action.tool, "integration_query");
    assert_eq!(
        disconnected_content["error_code"].as_str(),
        Some("not_connected"),
        "live empty workspace must be not_connected (not internal / unknown_op): {disconnected_content}"
    );
    assert_no_token_echo(
        "disconnected execute_tool Value",
        &serde_json::to_string(&disconnected_content).expect("serialize disconnected content"),
    );
    assert_no_token_echo(
        "disconnected ChatAction",
        &serde_json::to_string(&disconnected_action).expect("serialize disconnected action"),
    );

    assert_ne!(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", TEST_JWT_SECRET,
        "seal key A must differ from state.integrations.credentials_key"
    );
    let key_a = CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("key A");
    let sealed_wrong = key_a
        .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
        .expect("seal under key A");
    let conn_id = Uuid::now_v7();
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: conn_id,
            workspace_id,
            provider: "github".into(),
            account_label: Some("iqdec".into()),
            config: json!({ "site": "github.com" }),
            credentials: sealed_wrong,
            connected_by: Some(user_id),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("upsert github sealed under key A");

    let (decrypt_content, decrypt_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "integration_query",
        &json!({
            "provider": "github",
            "operation": "list_repos",
            "args": stuffed_args,
        }),
        &llm,
    )
    .await;
    assert!(
        !decrypt_action.ok,
        "wrong key must be ok:false: action={decrypt_action:?} content={decrypt_content}"
    );
    assert_eq!(
        decrypt_content["error_code"].as_str(),
        Some("decrypt_failed"),
        "state key must not decrypt key-A envelope: {decrypt_content}"
    );
    let decrypt_summary = decrypt_content["summary"].as_str().unwrap_or("");
    assert!(
        decrypt_summary.contains("failed to decrypt credentials")
            && !decrypt_summary.contains(LEAK),
        "decrypt_failed summary must be the static string and must not contain LEAK: {decrypt_content}"
    );
    assert!(
        !decrypt_action.summary.contains(LEAK),
        "ChatAction summary must not contain LEAK: {}",
        decrypt_action.summary
    );
    assert_no_token_echo(
        "decrypt_failed execute_tool Value",
        &serde_json::to_string(&decrypt_content).expect("serialize decrypt content"),
    );
    assert_no_token_echo(
        "decrypt_failed ChatAction",
        &serde_json::to_string(&decrypt_action).expect("serialize decrypt action"),
    );

    let sealed_ok = state
        .integrations
        .credentials_key
        .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
        .expect("seal under state key");
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: conn_id,
            workspace_id,
            provider: "github".into(),
            account_label: Some("iqdec".into()),
            config: json!({ "site": "github.com" }),
            credentials: sealed_ok,
            connected_by: Some(user_id),
            at: chrono::Utc::now(),
        },
    )
    .await
    .expect("re-upsert github sealed under state key");

    let (control_content, control_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "integration_query",
        &json!({
            "provider": "github",
            "operation": "list_repos",
            "args": stuffed_args,
        }),
        &llm,
    )
    .await;
    let control_code = control_content["error_code"].as_str();
    assert_ne!(
        control_code,
        Some("decrypt_failed"),
        "matching state key must decrypt (not a tautology that every invoke is decrypt_failed): {control_content}"
    );
    assert_ne!(
        control_code,
        Some("not_connected"),
        "matching state key must see the connected row: {control_content}"
    );
    assert_ne!(
        control_code,
        Some("unknown_op"),
        "list_repos is allowlisted: {control_content}"
    );
    assert_no_token_echo(
        "control execute_tool Value",
        &serde_json::to_string(&control_content).expect("serialize control content"),
    );
    assert_no_token_echo(
        "control ChatAction",
        &serde_json::to_string(&control_action).expect("serialize control action"),
    );
}

/// Docs §7.1: `launch_agent_task` / `launch_agent_plan` titles must be
/// non-empty and ≤ 200 chars. Empty, whitespace-only, and 201-char titles
/// fail closed on `execute_tool` (ok:false, no run_id) and insert no
/// accepted plan/run or board task. Distinct from the 1000-task cap.
/// Exactly 200 and a unique short title still launch.
#[tokio::test]
async fn launch_title_execute_tool_rejects_empty_and_over_200_and_inserts_no_run() {
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
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);

    let snap_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "snapshot tasks: {}",
        snap_tasks.body
    );
    let snap_task_ids = fleet_task_ids(&snap_tasks.body);
    let snap_task_titles = fleet_task_titles(&snap_tasks.body);

    fn title_err_is_empty_or_too_long(err: &str, raw: &str) {
        let lower = err.to_ascii_lowercase();
        if raw.trim().is_empty() {
            assert!(
                lower.contains("empty") && lower.contains("title"),
                "blank title must fail as empty, not the 1000-task cap: {err}"
            );
        } else {
            assert!(
                lower.contains("title")
                    && (lower.contains("too long")
                        || lower.contains("max")
                        || lower.contains("200")),
                "201-char title must fail the 200-char title cap (not the task cap): {err}"
            );
        }
        assert!(
            !lower.contains("too many") && !lower.contains("1000"),
            "title length is not the 1000-task cap: {err}"
        );
    }

    fn assert_launch_title_rejected(
        tool: &str,
        raw: &str,
        action: &fleet_server::chat_tools::ChatAction,
        content: &Value,
    ) {
        assert!(
            !action.ok,
            "{tool} title {raw:?} must fail closed (ok:false): {action:?} {content}"
        );
        assert_eq!(action.tool, tool, "{tool} action.tool: {action:?}");
        assert_eq!(
            action.run_id, None,
            "{tool} title {raw:?} must not return a run_id: {action:?}"
        );
        assert!(
            content.get("run_id").and_then(|v| v.as_str()).is_none(),
            "{tool} body must not include a run_id: {content}"
        );
        let err = content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str());
        title_err_is_empty_or_too_long(err, raw);
    }

    async fn assert_store_unchanged(
        app: &axum::Router,
        jar: &CookieJar,
        ws: &str,
        snap_ids: &[String],
        snap_task_ids: &[String],
        snap_task_titles: &[String],
        forbidden_titles: &[&str],
        label: &str,
    ) {
        let runs = call(app, "GET", &fleet(ws, "/runs"), Some(jar), None).await;
        assert_eq!(runs.status, StatusCode::OK, "{label} runs: {}", runs.body);
        let after_ids = fleet_run_ids(&runs.body);
        assert_eq!(
            after_ids, snap_ids,
            "{label} GET /fleet/runs must not grow: snap={snap_ids:?} after={after_ids:?} body={}",
            runs.body
        );

        let tasks = call(app, "GET", &fleet(ws, "/tasks"), Some(jar), None).await;
        assert_eq!(
            tasks.status,
            StatusCode::OK,
            "{label} tasks: {}",
            tasks.body
        );
        let after_task_ids = fleet_task_ids(&tasks.body);
        assert_eq!(
            after_task_ids, snap_task_ids,
            "{label} GET /fleet/tasks must not grow a board task: snap={snap_task_ids:?} after={after_task_ids:?} body={}",
            tasks.body
        );
        let after_titles = fleet_task_titles(&tasks.body);
        for title in forbidden_titles {
            if snap_task_titles.iter().any(|t| t == title) {
                continue;
            }
            assert!(
                !after_titles.iter().any(|t| t == title),
                "{label} GET /fleet/tasks must not grow a board task titled {title:?}: {}",
                tasks.body
            );
        }
    }

    let over = "x".repeat(201);
    assert_eq!(over.len(), 201);
    let bads = ["", "   ", over.as_str()];

    for bad in bads {
        let (content, action) = fleet_server::chat_tools::execute_tool(
            &state,
            workspace_id,
            user_id,
            "launch_agent_task",
            &json!({ "title": bad, "spec": "do it" }),
            &llm,
        )
        .await;
        assert_launch_title_rejected("launch_agent_task", bad, &action, &content);
        assert_store_unchanged(
            &app,
            &jar,
            &ws,
            &snap_ids,
            &snap_task_ids,
            &snap_task_titles,
            &[bad],
            &format!("launch_agent_task title {bad:?}"),
        )
        .await;
    }

    for bad in bads {
        let (content, action) = fleet_server::chat_tools::execute_tool(
            &state,
            workspace_id,
            user_id,
            "launch_agent_plan",
            &json!({
                "tasks": [{ "id": "t1", "title": bad, "spec": "do it" }]
            }),
            &llm,
        )
        .await;
        assert_launch_title_rejected("launch_agent_plan", bad, &action, &content);
        let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
        assert_eq!(
            runs.status,
            StatusCode::OK,
            "launch_agent_plan title {bad:?} runs: {}",
            runs.body
        );
        let after_ids = fleet_run_ids(&runs.body);
        assert_eq!(
            after_ids, snap_ids,
            "launch_agent_plan title {bad:?} GET /fleet/runs must still be the snapshot: snap={snap_ids:?} after={after_ids:?} body={}",
            runs.body
        );
    }

    let short_title = format!("launch-title-ok-{}", Uuid::new_v4());
    let (short_content, short_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": short_title,
            "spec": "Settle under stub after title-gate rejects.",
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        short_action.ok,
        "unique short launch_agent_task title must still succeed: {short_action:?} {short_content}"
    );
    let short_run = short_action
        .run_id
        .as_deref()
        .unwrap_or_else(|| panic!("short launch must return run_id: {short_action:?}"));
    assert!(
        Uuid::parse_str(short_run).is_ok(),
        "short launch run_id must be a uuid: {short_run:?} {short_action:?}"
    );
    let after_short = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_short.status,
        StatusCode::OK,
        "runs after short launch: {}",
        after_short.body
    );
    let after_short_ids = fleet_run_ids(&after_short.body);
    assert!(
        after_short_ids.iter().any(|id| id == short_run),
        "GET /fleet/runs must list short-title run {short_run}: {}",
        after_short.body
    );

    let prefix = format!("T200-{}-", Uuid::new_v4());
    assert!(prefix.len() < 200, "200-char unique prefix too long");
    let at_cap = format!("{prefix}{}", "x".repeat(200 - prefix.len()));
    assert_eq!(at_cap.len(), 200, "control title must be exactly 200 chars");
    let (cap_content, cap_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": at_cap,
            "spec": "Exactly 200-char title must still launch.",
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        cap_action.ok,
        "exactly 200-char launch_agent_task title must still succeed: {cap_action:?} {cap_content}"
    );
    let cap_run = cap_action
        .run_id
        .as_deref()
        .unwrap_or_else(|| panic!("200-char launch must return run_id: {cap_action:?}"));
    assert!(
        Uuid::parse_str(cap_run).is_ok(),
        "200-char launch run_id must be a uuid: {cap_run:?} {cap_action:?}"
    );
    let after_cap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_cap.status,
        StatusCode::OK,
        "runs after 200-char launch: {}",
        after_cap.body
    );
    let after_cap_ids = fleet_run_ids(&after_cap.body);
    assert!(
        after_cap_ids.iter().any(|id| id == cap_run),
        "GET /fleet/runs must list 200-char-title run {cap_run}: {}",
        after_cap.body
    );
}

/// Docs §16: Chat `context_from` GitHub while not connected → tool error;
/// model tells the user to Connect. `launch_agent_task` / `launch_agent_plan`
/// must be `ok:false` and insert no accepted plan/run. Must not launch
/// anyway with `Failed (not_connected)` baked into `spec_md`. Prefetched
/// markdown / tool payload must never contain `access_token` / `refresh_token`.
#[tokio::test]
async fn context_from_disconnected_github_is_tool_error_and_inserts_no_plan() {
    const LEAK: &str = "gho_CTX_FROM_HTTP_MUST_NOT_ECHO_9d2a";

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
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    let (listed, list_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_integrations",
        &json!({}),
        &llm,
    )
    .await;
    assert!(
        list_action.ok,
        "list_integrations: {list_action:?} {listed}"
    );
    let github = listed["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == "github")
        .unwrap_or_else(|| panic!("github catalog row: {listed}"));
    assert_eq!(
        github["connected"], false,
        "fresh workspace must not have GitHub connected: {github}"
    );

    fn assert_no_token_echo(label: &str, blob: &str) {
        assert!(
            !blob.contains("access_token")
                && !blob.contains("refresh_token")
                && !blob.contains(LEAK),
            "{label} must not contain access_token/refresh_token: {blob}"
        );
    }

    fn assert_disconnected_tool_error(
        tool: &str,
        action: &fleet_server::chat_tools::ChatAction,
        content: &Value,
    ) {
        assert!(
            !action.ok,
            "{tool} context_from GitHub while not connected must be a tool error (ok:false), \
             not a launched run: {action:?} {content}"
        );
        assert_eq!(action.tool, tool, "{tool} action.tool: {action:?}");
        assert_eq!(
            action.run_id, None,
            "{tool} must not return a run_id when context_from is not connected: {action:?}"
        );
        assert!(
            action
                .sandbox_ids
                .as_ref()
                .map(|v| v.is_empty())
                .unwrap_or(true),
            "{tool} must not spawn sandboxes: {action:?}"
        );
        assert!(
            content.get("run_id").and_then(|v| v.as_str()).is_none(),
            "{tool} body must not include a run_id: {content}"
        );
        let err = content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str());
        let lower = err.to_ascii_lowercase();
        assert!(
            lower.contains("not_connected")
                || lower.contains("not connected")
                || lower.contains("connect"),
            "{tool} error must tell the model to Connect: {err} {content}"
        );
        let blob = format!("{action:?}{content}");
        assert_no_token_echo(tool, &blob);
        assert!(
            !blob.contains("Failed (not_connected)"),
            "{tool} must not bake Failed (not_connected) into the tool result as if the run launched: {blob}"
        );
    }

    let (task_content, task_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": "Read GitHub issues",
            "spec": "Summarize open issues.",
            "context_from": [{
                "provider": "github",
                "operation": "list_issues",
                "args": {
                    "owner": "acme",
                    "repo": "app",
                    "access_token": LEAK,
                    "refresh_token": LEAK
                }
            }]
        }),
        &llm,
    )
    .await;
    assert_disconnected_tool_error("launch_agent_task", &task_action, &task_content);

    let (plan_content, plan_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "title": "Read GitHub repos",
            "context_from": [{
                "provider": "github",
                "operation": "list_repos",
                "args": { "access_token": LEAK, "refresh_token": LEAK }
            }],
            "tasks": [{
                "id": "t1",
                "title": "Summarize repos",
                "spec": "List connected GitHub repos."
            }]
        }),
        &llm,
    )
    .await;
    assert_disconnected_tool_error("launch_agent_plan", &plan_action, &plan_content);

    let store_runs = state
        .orch
        .store
        .list_runs(workspace_id)
        .await
        .expect("list_runs");
    assert!(
        store_runs.is_empty(),
        "disconnected context_from must insert no accepted plan/run: {store_runs:?}"
    );

    let snap = state
        .orch
        .store
        .load_board_snapshot(workspace_id)
        .await
        .expect("board snapshot");
    assert!(
        snap.tasks.is_empty(),
        "disconnected context_from must insert no board tasks / spec_md: {:?}",
        snap.tasks
            .iter()
            .map(|t| (
                t.plan_task_id.as_str(),
                t.spec_md.as_str(),
                t.status.as_str()
            ))
            .collect::<Vec<_>>()
    );
    for t in &snap.tasks {
        assert!(
            !t.spec_md.contains("Failed (not_connected)"),
            "must not launch with Failed (not_connected) baked into spec_md: {}",
            t.spec_md
        );
        assert_no_token_echo("spec_md", &t.spec_md);
    }

    let (listed_runs, listed_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_runs",
        &json!({}),
        &llm,
    )
    .await;
    assert!(
        listed_action.ok,
        "list_runs after failed context_from: {listed_action:?} {listed_runs}"
    );
    assert_eq!(
        listed_runs["count"].as_u64().unwrap_or(u64::MAX),
        0,
        "list_runs must stay empty (no accepted plan/run): {listed_runs}"
    );

    let http_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(http_runs.status, StatusCode::OK, "{}", http_runs.body);
    let run_list = http_runs.body["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        run_list.is_empty(),
        "BFF /runs must not show a launch that baked not_connected into spec: {}",
        http_runs.body
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
        &[],
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

/// Docs §8.2: `GET /v1/projects/{id}/tasks/{task}` is the full task
/// (`spec_md`, HITL notes). `POST …/tasks/{task}/resume` is admin HITL
/// (409 if not blocked). No UI. A session cookie must not pass `AdminAuth`
/// on either path — cookie BFF is membership-scoped; `/v1` is not.
/// Distinct from [`v1_admin_cookie_cannot_get_patch_tick_or_cancel`]
/// (list /runs /workers /config /fixture /tick /cancel only — never
/// `GET /tasks/{task_id}` or `POST /v1` resume),
/// [`cookie_resume_blocked_task_promotes_to_ready_other_workspace_403`]
/// (cookie BFF `POST /fleet/tasks/{id}/resume`),
/// [`admin_token_unset_disables_v1_even_with_bearer_or_cookie`], and
/// worker heartbeat/status cookie isolation. Filter `-- v1_task_resume_cookie`.
#[tokio::test]
async fn v1_task_resume_cookie_cannot_get_detail_or_post_admin_resume() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let name = format!("v1-task-resume-cookie-{}", Uuid::new_v4());
    let title = format!("v1-task-resume-cookie-title-{}", Uuid::new_v4());
    let spec_md = format!(
        "# v1-task-resume-cookie-spec-{}\n\nDo not leak this spec_md on a cookie /v1 GET.\n",
        Uuid::new_v4()
    );
    let resume_msg = format!("v1-task-resume-cookie-hitl-{}", Uuid::new_v4());

    let created = call_bearer(
        &app,
        "POST",
        "/v1/projects",
        TEST_ADMIN_TOKEN,
        Some(json!({ "name": name, "max_agents": 2 })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN POST /v1/projects: {}",
        created.body
    );
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("project id: {}", created.body))
        .to_string();

    let plan = json!({
        "plan_id": Uuid::new_v4(),
        "project_id": pid,
        "run_id": Uuid::new_v4(),
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [{
            "plan_task_id": "t1",
            "title": title,
            "spec_md": spec_md,
            "depends_on": [],
            "worker_profile": "coding",
            "priority": 100
        }],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    let fx = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        TEST_ADMIN_TOKEN,
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN POST /v1/.../runs/fixture: {}",
        fx.body
    );

    let tick = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        tick.status,
        StatusCode::OK,
        "ADMIN_TOKEN POST /admin/tick to materialize fixture: {}",
        tick.body
    );

    let listed = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "ADMIN_TOKEN GET /v1/.../tasks: {}",
        listed.body
    );
    let tid = listed.body["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["title"].as_str() == Some(title.as_str()))
        .and_then(|t| t["id"].as_str())
        .unwrap_or_else(|| panic!("fixture task id for {title}: {}", listed.body))
        .to_string();

    let snap = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks/{tid}"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "ADMIN_TOKEN GET /v1/.../tasks/{{tid}}: {}",
        snap.body
    );
    assert_eq!(
        snap.body["title"].as_str(),
        Some(title.as_str()),
        "snapshot title: {}",
        snap.body
    );
    assert_eq!(
        snap.body["spec_md"].as_str(),
        Some(spec_md.as_str()),
        "snapshot spec_md: {}",
        snap.body
    );
    assert!(
        snap.body["hitl_resume_requested_at"].is_null(),
        "fresh fixture must not already have hitl_resume_requested_at: {}",
        snap.body
    );

    let get_uri = format!("/v1/projects/{pid}/tasks/{tid}");
    let resume_uri = format!("/v1/projects/{pid}/tasks/{tid}/resume");
    let secrets = [title.as_str(), spec_md.as_str(), resume_msg.as_str()];

    let cookie_get = call(&app, "GET", &get_uri, Some(&jar), None).await;
    assert_v1_admin_cookie_rejected(&cookie_get, "GET", &get_uri, &secrets);
    assert!(
        !cookie_get.raw.contains("hitl_notes") && cookie_get.body.get("hitl_notes").is_none(),
        "cookie GET must not leak HITL notes: {}",
        cookie_get.raw
    );
    assert!(
        !cookie_get.raw.contains("spec_md") && cookie_get.body.get("spec_md").is_none(),
        "cookie GET must not leak spec_md: {}",
        cookie_get.raw
    );

    let cookie_resume = call(
        &app,
        "POST",
        &resume_uri,
        Some(&jar),
        Some(json!({ "message": resume_msg })),
    )
    .await;
    assert_v1_admin_cookie_rejected(&cookie_resume, "POST", &resume_uri, &secrets);
    assert_ne!(
        cookie_resume.body.get("resume_requested"),
        Some(&json!(true)),
        "cookie POST /v1 resume must not claim resume_requested: {}",
        cookie_resume.body
    );
    assert!(
        !cookie_resume.raw.contains("hitl_notes") && cookie_resume.body.get("hitl_notes").is_none(),
        "cookie POST /v1 resume must not leak HITL notes: {}",
        cookie_resume.raw
    );

    let after = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks/{tid}"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "admin GET /tasks/{{tid}} after cookie calls: {}",
        after.body
    );
    assert_eq!(
        after.body, snap.body,
        "cookie resume must not mutate the task (hitl_resume_requested_at / notes): snap={} after={}",
        snap.body, after.body
    );
    assert!(
        after.body["hitl_resume_requested_at"].is_null(),
        "cookie resume must not set hitl_resume_requested_at: {}",
        after.body
    );
    let notes = after.body["hitl_notes"].as_str().unwrap_or("");
    assert!(
        !notes.contains(&resume_msg),
        "cookie resume message must not stick: {}",
        after.body
    );

    let later_tick = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        later_tick.status,
        StatusCode::OK,
        "later ADMIN_TOKEN tick: {}",
        later_tick.body
    );
    let after_tick = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks/{tid}"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        after_tick.status,
        StatusCode::OK,
        "ADMIN_TOKEN GET /tasks/{{tid}} after later tick: {}",
        after_tick.body
    );
    assert!(
        after_tick.body["hitl_resume_requested_at"].is_null(),
        "later admin tick must not have seen a cookie resume intent: {}",
        after_tick.body
    );
    let notes_after_tick = after_tick.body["hitl_notes"].as_str().unwrap_or("");
    assert!(
        !notes_after_tick.contains(&resume_msg),
        "later admin tick must not apply the cookie HITL message: {}",
        after_tick.body
    );
    assert_eq!(
        after_tick.body["title"].as_str(),
        Some(title.as_str()),
        "later tick must still return the snapshot title: {}",
        after_tick.body
    );
    assert_eq!(
        after_tick.body["spec_md"].as_str(),
        Some(spec_md.as_str()),
        "later tick must still return snapshot spec_md: {}",
        after_tick.body
    );

    let admin_get = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks/{tid}"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        admin_get.status,
        StatusCode::OK,
        "ADMIN_TOKEN GET /tasks/{{tid}} must still 200: {}",
        admin_get.body
    );

    let bff_fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        bff_fx.status,
        StatusCode::CREATED,
        "cookie POST /api/workspaces/{{ws}}/fleet/runs/fixture: {}",
        bff_fx.body
    );
    let _ = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    let cookie_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        cookie_tasks.status,
        StatusCode::OK,
        "cookie GET /fleet/tasks: {}",
        cookie_tasks.body
    );
    let cookie_task = cookie_tasks.body["tasks"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("cookie fixture task id: {}", cookie_tasks.body))
        .to_string();
    let bff_resume = call(
        &app,
        "POST",
        &fleet(&ws, &format!("/tasks/{cookie_task}/resume")),
        Some(&jar),
        Some(json!({ "message": "go" })),
    )
    .await;
    assert_eq!(
        bff_resume.status,
        StatusCode::CONFLICT,
        "cookie POST /api/workspaces/{{ws}}/fleet/tasks/{{id}}/resume on a non-blocked fixture task must stay 409 (not a tautology that every resume 401s): {}",
        bff_resume.body
    );
    let bff_err = bff_resume.body["error"].as_str().unwrap_or("");
    assert!(
        bff_err.to_ascii_lowercase().contains("not blocked"),
        "cookie BFF non-blocked resume 409 must say task is not blocked: {}",
        bff_resume.body
    );
}

/// Docs §8.2: `/v1` admin is Bearer `ADMIN_TOKEN`. A session cookie must not
/// GET, PATCH, tick, fixture, or cancel project routes (that would leak
/// `spec_md` or tick another project by UUID). Cookie BFF stays
/// `/api/workspaces/{id}/fleet/*`. Distinct from
/// [`v1_projects_rejects_missing_bearer_and_session_cookie`] (create only)
/// and [`cookie_cancel_foreign_run_id_does_not_mark_cancelled`] (cookie
/// `/v1` cancel 401 kept). Filter `-- v1_admin_cookie`.
#[tokio::test]
async fn v1_admin_cookie_cannot_get_patch_tick_or_cancel() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let name = format!("v1-admin-cookie-{}", Uuid::new_v4());
    let title_root = format!("v1-admin-cookie-root-{}", Uuid::new_v4());
    let title_child = format!("v1-admin-cookie-child-{}", Uuid::new_v4());
    let spec_md = format!(
        "# v1-admin-cookie-spec-{}\n\nDo not leak this spec_md on a cookie /v1 call.\n",
        Uuid::new_v4()
    );
    let cookie_repo = format!(
        "https://github.com/acme/v1-admin-cookie-{}.git",
        Uuid::new_v4()
    );

    let created = call_bearer(
        &app,
        "POST",
        "/v1/projects",
        TEST_ADMIN_TOKEN,
        Some(json!({ "name": name, "max_agents": 2 })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN POST /v1/projects: {}",
        created.body
    );
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("project id: {}", created.body))
        .to_string();
    assert_eq!(
        created.body["name"].as_str(),
        Some(name.as_str()),
        "created project name: {}",
        created.body
    );

    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let plan = json!({
        "plan_id": plan_id,
        "project_id": pid,
        "run_id": run_id,
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [
            {
                "plan_task_id": "t1",
                "title": title_root,
                "spec_md": spec_md,
                "depends_on": [],
                "worker_profile": "coding",
                "priority": 100
            },
            {
                "plan_task_id": "t2",
                "title": title_child,
                "spec_md": spec_md,
                "depends_on": ["t1"],
                "worker_profile": "coding",
                "priority": 50
            }
        ],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    let fx = call_bearer(
        &app,
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        TEST_ADMIN_TOKEN,
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "ADMIN_TOKEN POST /v1/.../runs/fixture: {}",
        fx.body
    );
    let rid = fx.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("run_id: {}", fx.body))
        .to_string();
    assert_eq!(rid, run_id.to_string(), "fixture run_id: {}", fx.body);

    let snap_tasks = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "pre-call admin GET /tasks: {}",
        snap_tasks.body
    );
    let snap_config = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/config"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        snap_config.status,
        StatusCode::OK,
        "pre-call admin GET /config: {}",
        snap_config.body
    );
    assert_ne!(
        snap_config.body["repo_url"].as_str(),
        Some(cookie_repo.as_str()),
        "pre-call config must not already have the cookie PATCH url: {}",
        snap_config.body
    );

    let work_dir_needle = format!("projects/{pid}/sessions");
    let secrets = [
        name.as_str(),
        title_root.as_str(),
        title_child.as_str(),
        spec_md.as_str(),
        work_dir_needle.as_str(),
    ];

    let v1 = format!("/v1/projects/{pid}");
    let cookie_calls = [
        ("GET", v1.clone(), None),
        ("GET", format!("{v1}/tasks"), None),
        ("GET", format!("{v1}/runs"), None),
        ("GET", format!("{v1}/workers"), None),
        ("GET", format!("{v1}/config"), None),
        (
            "PATCH",
            format!("{v1}/config"),
            Some(json!({ "repo_url": cookie_repo })),
        ),
        (
            "POST",
            format!("{v1}/runs/fixture"),
            Some(json!({
                "plan": {
                    "plan_id": Uuid::new_v4(),
                    "project_id": pid,
                    "run_id": Uuid::new_v4(),
                    "version": 1,
                    "status": "accepted",
                    "source": "cookie-must-not-fixture",
                    "tasks": [{
                        "plan_task_id": "cookie-t1",
                        "title": "cookie-must-not-create",
                        "depends_on": [],
                        "worker_profile": "coding",
                        "priority": 1
                    }],
                    "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
                }
            })),
        ),
        ("POST", format!("{v1}/admin/tick"), None),
        (
            "POST",
            format!("{v1}/runs/{rid}/cancel"),
            Some(json!({ "reason": "cookie-v1-admin-cancel" })),
        ),
    ];

    for (method, uri, body) in &cookie_calls {
        let out = call(&app, method, uri, Some(&jar), body.clone()).await;
        assert_v1_admin_cookie_rejected(&out, method, uri, &secrets);
    }

    let after_tasks = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        after_tasks.status,
        StatusCode::OK,
        "admin GET /tasks after cookie calls: {}",
        after_tasks.body
    );
    assert_eq!(
        after_tasks.body, snap_tasks.body,
        "cookie tick must not spawn / cookie fixture must not materialize: snap={} after={}",
        snap_tasks.body, after_tasks.body
    );

    let after_config = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/config"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        after_config.status,
        StatusCode::OK,
        "admin GET /config after cookie calls: {}",
        after_config.body
    );
    assert_eq!(
        after_config.body, snap_config.body,
        "cookie PATCH must not write repo_url: snap={} after={}",
        snap_config.body, after_config.body
    );
    assert_ne!(
        after_config.body["repo_url"].as_str(),
        Some(cookie_repo.as_str()),
        "cookie PATCH repo_url must not persist: {}",
        after_config.body
    );

    let after_runs = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}/runs"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        after_runs.status,
        StatusCode::OK,
        "admin GET /runs after cookie calls: {}",
        after_runs.body
    );
    let runs = after_runs.body["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        runs.len(),
        1,
        "cookie POST /runs/fixture must not insert a run: {}",
        after_runs.body
    );
    assert_eq!(
        runs[0]["id"].as_str(),
        Some(rid.as_str()),
        "admin fixture run must remain the only run: {}",
        after_runs.body
    );
    assert!(
        runs[0]["cancel_requested_at"].is_null(),
        "cookie POST /runs/{{rid}}/cancel must not mark cancel: {}",
        after_runs.body
    );

    let admin_get = call_bearer(
        &app,
        "GET",
        &format!("/v1/projects/{pid}"),
        TEST_ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(
        admin_get.status,
        StatusCode::OK,
        "ADMIN_TOKEN GET /v1/projects/{{pid}} must still 200: {}",
        admin_get.body
    );
    assert_eq!(
        admin_get.body["name"].as_str(),
        Some(name.as_str()),
        "control GET must still return the project name: {}",
        admin_get.body
    );

    let bff_fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        bff_fx.status,
        StatusCode::CREATED,
        "cookie POST /api/workspaces/{{ws}}/fleet/runs/fixture must still 201: {}",
        bff_fx.body
    );
}

fn assert_v1_admin_cookie_rejected(out: &HttpOut, method: &str, uri: &str, secrets: &[&str]) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "session cookie must not authorize {method} {uri}: {}",
        out.body
    );
    let hay = format!("{} {}", out.body, out.raw);
    let hay_l = hay.to_ascii_lowercase();
    assert!(
        hay_l.contains("invalid admin token"),
        "{method} {uri} must say invalid admin token (ADMIN_TOKEN is configured): {}",
        out.raw
    );
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        assert!(
            !hay.contains(secret),
            "{method} {uri} must not leak {secret:?}: {}",
            out.raw
        );
    }
    assert_ne!(
        out.body.get("cancel_requested"),
        Some(&json!(true)),
        "{method} {uri} must not claim cancel_requested: {}",
        out.body
    );
    assert!(
        out.body.get("tick_id").is_none() || out.body["tick_id"].is_null(),
        "{method} {uri} must not return a tick_id: {}",
        out.body
    );
    assert!(
        out.body.get("spawned").is_none() || out.body["spawned"].is_null(),
        "{method} {uri} must not return spawned: {}",
        out.body
    );
}

/// Unset ADMIN_TOKEN disables /v1 even with a session cookie or Bearer.
/// Cookie `/api/workspaces/{id}/fleet/*` does not use ADMIN_TOKEN.
#[tokio::test]
async fn admin_token_unset_disables_v1_even_with_bearer_or_cookie() {
    let (app, state) = app_with_state_admin_token("").await;
    assert_eq!(
        state.orch.auth_mode,
        AuthMode::Token,
        "unset ADMIN_TOKEN must stay AuthMode::Token (None skips the /v1 gate)"
    );
    assert!(
        state.orch.admin_token.trim().is_empty(),
        "fixture must clone app() with empty admin_token, got {:?}",
        state.orch.admin_token
    );

    let (_email, jar, ws) = signup_and_login(&app).await;
    let body = json!({
        "name": format!("v1-unset-{}", Uuid::new_v4()),
        "max_agents": 2
    });

    let no_auth = call(&app, "POST", "/v1/projects", None, Some(body.clone())).await;
    assert_v1_admin_token_unset_rejected(&no_auth, "no auth");

    let cookie_only = call(&app, "POST", "/v1/projects", Some(&jar), Some(body.clone())).await;
    assert_v1_admin_token_unset_rejected(&cookie_only, "session cookie");

    let empty_bearer = call_inner(
        &app,
        "POST",
        "/v1/projects",
        None,
        Some(""),
        &[],
        Some(body.clone()),
    )
    .await;
    assert_v1_admin_token_unset_rejected(&empty_bearer, "Authorization: Bearer (empty)");

    let bearer_x = call_inner(
        &app,
        "POST",
        "/v1/projects",
        None,
        Some("x"),
        &[],
        Some(body),
    )
    .await;
    assert_v1_admin_token_unset_rejected(&bearer_x, "Authorization: Bearer x");

    // Cookie BFF still works — this is not a tautology that every POST 401s.
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "cookie POST /api/workspaces/{{ws}}/fleet/runs/fixture must still 201 when ADMIN_TOKEN is unset: {}",
        fx.body
    );
}

fn assert_v1_admin_token_unset_rejected(out: &HttpOut, label: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{label} POST /v1/projects must 401 when ADMIN_TOKEN is unset: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{label} POST /v1/projects must not 201 a project: {}",
        out.body
    );
    let hay = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        hay.contains("admin automation disabled") || hay.contains("invalid admin token"),
        "{label} must say admin automation disabled or invalid admin token: {}",
        out.raw
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
    let sid = sessions.body["sessions"][0]["id"]
        .as_str()
        .expect("session");
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

/// Docs §8.1: cookie BFF `/api/workspaces/{id}/fleet/*` requires a valid
/// member. B's cookie on A's tick / fixture / settings / config / tasks /
/// runs / workers / personas is 403 not-a-member and must not mutate A's
/// board. Distinct from `fleet_requires_membership` (GET /fleet and GET
/// session detail only). Filter `-- fleet_tick_fixture_other_ws`.
/// catalog:list-fleet-tick-fixture-other-ws-403
fn assert_fleet_tick_fixture_other_ws_forbidden(out: &HttpOut, label: &str, leaks: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not be 200: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{label} must not be 201: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    for key in [
        "tasks",
        "runs",
        "sandboxes",
        "workers",
        "personas",
        "sessions",
    ] {
        if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
            assert!(
                arr.is_empty(),
                "{label} must not include A's {key}: {}",
                out.body
            );
        }
    }
    assert!(
        out.body.get("tick_id").is_none() || out.body["tick_id"].is_null(),
        "{label} must not return a tick report: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "{label} must not return a run_id: {}",
        out.body
    );
    if let Some(spawned) = out.body.get("spawned") {
        assert_eq!(spawned, 0, "{label} must not report spawned: {}", out.body);
    }
    assert!(
        out.body.get("max_agents").is_none(),
        "{label} must not return applied settings: {}",
        out.body
    );
    if let Some(repo) = out.body.get("repo_url") {
        assert!(
            repo.is_null() || repo.as_str().map(|s| s.is_empty()).unwrap_or(false),
            "{label} must not return repo_url: {}",
            out.body
        );
    }
    if let Some(cfg) = out.body.get("config") {
        if let Some(repo) = cfg.get("repo_url") {
            assert!(
                repo.is_null() || repo.as_str().map(|s| s.is_empty()).unwrap_or(false),
                "{label} must not return config.repo_url: {}",
                out.body
            );
        }
    }
    for secret in leaks {
        if secret.is_empty() {
            continue;
        }
        assert!(
            !out.raw.contains(secret),
            "{label} body must not contain {secret:?}: {}",
            out.raw
        );
        assert!(
            !out.body.to_string().contains(secret),
            "{label} json must not contain {secret:?}: {}",
            out.body
        );
    }
}

#[tokio::test]
async fn fleet_tick_fixture_other_ws_is_403_and_does_not_mutate() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(fx.status, StatusCode::CREATED, "{}", fx.body);
    assert_eq!(fx.body["kind"], "chain", "{}", fx.body);
    let run_id = fx.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("chain fixture run_id: {}", fx.body))
        .to_string();
    let plan_id = fx.body["plan_id"]
        .as_str()
        .unwrap_or_else(|| panic!("chain fixture plan_id: {}", fx.body))
        .to_string();

    let snap_tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "A GET /tasks: {}",
        snap_tasks.body
    );
    let snap_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        snap_runs.status,
        StatusCode::OK,
        "A GET /runs: {}",
        snap_runs.body
    );
    let snap_workers = call(&app, "GET", &fleet(&a_ws, "/workers"), Some(&a_jar), None).await;
    assert_eq!(
        snap_workers.status,
        StatusCode::OK,
        "A GET /workers: {}",
        snap_workers.body
    );
    let snap_settings = call(&app, "GET", &fleet(&a_ws, "/settings"), Some(&a_jar), None).await;
    assert_eq!(
        snap_settings.status,
        StatusCode::OK,
        "A GET /settings: {}",
        snap_settings.body
    );

    let snap_run_ids = fleet_run_ids(&snap_runs.body);
    assert!(
        snap_run_ids.iter().any(|id| id == &run_id),
        "A snapshot GET /runs must list fixture {run_id} so the leak check is not vacuous: {}",
        snap_runs.body
    );
    assert_eq!(
        snap_run_ids.len(),
        1,
        "A snapshot must be the one chain plan so a second B fixture is visible: {:?}",
        snap_run_ids
    );
    assert_ne!(
        snap_settings.body["max_agents"], 2,
        "snapshot max_agents must not already be 2 so B PATCH is detectable: {}",
        snap_settings.body
    );
    const STOLEN_REPO: &str = "https://example.test/other.git";
    assert_ne!(
        snap_settings.body["repo_url"].as_str(),
        Some(STOLEN_REPO),
        "snapshot repo_url must not already be the stolen URL: {}",
        snap_settings.body
    );

    let spec_root = "# Root task\n\nWrite a short plan artifact, then succeed so `t2` can run.\n";
    let spec_child = "# Child task\n\nDepends on `t1`. Produce a final summary artifact.\n";
    let snap_titles = fleet_task_titles(&snap_tasks.body);
    let worker_dirs: Vec<String> = snap_workers.body["sandboxes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["work_dir"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut leaks: Vec<&str> = vec![
        "Root task",
        "Child task",
        run_id.as_str(),
        plan_id.as_str(),
        spec_root,
        spec_child,
    ];
    for title in &snap_titles {
        if !title.is_empty() {
            leaks.push(title.as_str());
        }
    }
    for dir in &worker_dirs {
        leaks.push(dir.as_str());
    }

    let as_b = [
        ("B POST A's /tick", "POST", fleet(&a_ws, "/tick"), None),
        (
            "B POST A's /runs/fixture",
            "POST",
            fleet(&a_ws, "/runs/fixture"),
            Some(json!({ "kind": "chain" })),
        ),
        (
            "B PATCH A's /settings",
            "PATCH",
            fleet(&a_ws, "/settings"),
            Some(json!({ "max_agents": 2 })),
        ),
        (
            "B PATCH A's /config",
            "PATCH",
            fleet(&a_ws, "/config"),
            Some(json!({ "repo_url": STOLEN_REPO })),
        ),
        ("B GET A's /tasks", "GET", fleet(&a_ws, "/tasks"), None),
        ("B GET A's /runs", "GET", fleet(&a_ws, "/runs"), None),
        ("B GET A's /workers", "GET", fleet(&a_ws, "/workers"), None),
        (
            "B GET A's /fleet/personas",
            "GET",
            fleet(&a_ws, "/personas"),
            None,
        ),
    ];
    for (label, method, uri, body) in &as_b {
        let out = call(&app, method, uri, Some(&b_jar), body.clone()).await;
        assert_fleet_tick_fixture_other_ws_forbidden(&out, label, &leaks);
    }

    let after_tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        after_tasks.status,
        StatusCode::OK,
        "A GET /tasks after B: {}",
        after_tasks.body
    );
    assert_eq!(
        after_tasks.body, snap_tasks.body,
        "B tick must not spawn / materialize A's ready tasks: before={} after={}",
        snap_tasks.body, after_tasks.body
    );
    let after_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        after_runs.status,
        StatusCode::OK,
        "A GET /runs after B: {}",
        after_runs.body
    );
    assert_eq!(
        after_runs.body, snap_runs.body,
        "B fixture must not insert a second plan: before={} after={}",
        snap_runs.body, after_runs.body
    );
    assert_eq!(
        fleet_run_ids(&after_runs.body),
        snap_run_ids,
        "A run ids must stay the snapshot after B"
    );
    let after_settings = call(&app, "GET", &fleet(&a_ws, "/settings"), Some(&a_jar), None).await;
    assert_eq!(
        after_settings.status,
        StatusCode::OK,
        "A GET /settings after B: {}",
        after_settings.body
    );
    assert_eq!(
        after_settings.body, snap_settings.body,
        "B PATCH must not write max_agents or repo_url: before={} after={}",
        snap_settings.body, after_settings.body
    );
    assert_ne!(
        after_settings.body["max_agents"], 2,
        "B PATCH /settings must not store max_agents=2: {}",
        after_settings.body
    );
    assert_ne!(
        after_settings.body["repo_url"].as_str(),
        Some(STOLEN_REPO),
        "B PATCH /config must not store the stolen repo_url: {}",
        after_settings.body
    );
    let after_workers = call(&app, "GET", &fleet(&a_ws, "/workers"), Some(&a_jar), None).await;
    assert_eq!(
        after_workers.status,
        StatusCode::OK,
        "A GET /workers after B: {}",
        after_workers.body
    );
    assert_eq!(
        after_workers.body, snap_workers.body,
        "B tick must not spawn worker sessions: before={} after={}",
        snap_workers.body, after_workers.body
    );

    let a_tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        a_tasks.status,
        StatusCode::OK,
        "A cookie on A's GET /tasks must stay 200 so this is not a tautology: {}",
        a_tasks.body
    );

    let b_fx = call(
        &app,
        "POST",
        &fleet(&b_ws, "/runs/fixture"),
        Some(&b_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        b_fx.status,
        StatusCode::CREATED,
        "B cookie on B's POST /runs/fixture must still 201 so this is not a tautology that every fleet POST 403s: {}",
        b_fx.body
    );
    assert_eq!(b_fx.body["kind"], "chain", "{}", b_fx.body);
    assert_eq!(
        b_fx.body["project_id"].as_str(),
        Some(b_ws.as_str()),
        "B fixture is B's workspace: {}",
        b_fx.body
    );
    assert_ne!(
        b_fx.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "B's own fixture must be a distinct run: {}",
        b_fx.body
    );
}

/// Invite of an existing user joins immediately (role=member). That teammate
/// must use fleet/chat/cases/inbox on the owner's workspace; hosts + invites
/// stay owner-only; a third owner's workspace stays 403.
#[tokio::test]
async fn invited_member_can_use_workspace() {
    use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox};

    let (app, state) = app_with_state().await;
    let (a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (m_email, m_jar, _m_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;

    let invited = call(
        &app,
        "POST",
        &invites_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "email": m_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        invited.status,
        StatusCode::CREATED,
        "owner invite existing user: {}",
        invited.body
    );
    assert_eq!(
        invited.body["added_existing_user"], true,
        "existing user must join immediately: {}",
        invited.body
    );

    let member_gets = [
        ("/fleet", fleet(&a_ws, "")),
        ("/fleet/settings", fleet(&a_ws, "/settings")),
        ("/fleet/overview", fleet(&a_ws, "/overview")),
        (
            "/chat/status",
            format!("/api/workspaces/{a_ws}/chat/status"),
        ),
        ("/cases", cases_path(&a_ws, "")),
        ("/inbox", inbox_path(&a_ws, "")),
        ("/integrations", integrations_path(&a_ws)),
        ("/personas", personas_path(&a_ws)),
        ("/members", members_path(&a_ws)),
    ];
    for (label, uri) in &member_gets {
        let out = call(&app, "GET", uri, Some(&m_jar), None).await;
        assert_eq!(
            out.status,
            StatusCode::OK,
            "member GET {label} must be 200 not 403: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::FORBIDDEN,
            "member GET {label} must not 403 on their workspace: {}",
            out.body
        );
        assert_product_body_has_no_secrets(&out, Some(&m_jar), &format!("member GET {label}"));
    }

    let fleet_as_m = call(&app, "GET", &fleet(&a_ws, ""), Some(&m_jar), None).await;
    assert_eq!(
        fleet_as_m.body["workspace_id"].as_str(),
        Some(a_ws.as_str()),
        "member GET /fleet workspace_id: {}",
        fleet_as_m.body
    );
    assert_eq!(
        fleet_as_m.body["role"].as_str(),
        Some("member"),
        "invited user must be role=member: {}",
        fleet_as_m.body
    );

    let members = call(&app, "GET", &members_path(&a_ws), Some(&m_jar), None).await;
    let roster = members.body["members"]
        .as_array()
        .unwrap_or_else(|| panic!("members: {}", members.body));
    assert!(
        roster.iter().any(|m| {
            m["email"].as_str() == Some(a_email.as_str()) && m["role"].as_str() == Some("owner")
        }),
        "owner A must appear: {}",
        members.body
    );
    assert!(
        roster.iter().any(|m| {
            m["email"].as_str() == Some(m_email.as_str()) && m["role"].as_str() == Some("member")
        }),
        "invited M must appear as member: {}",
        members.body
    );
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("member"),
        "you.role: {}",
        members.body
    );

    let case_title = format!("member-case-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &cases_path(&a_ws, ""),
        Some(&m_jar),
        Some(json!({
            "title": case_title,
            "policy_md": "short"
        })),
    )
    .await;
    assert_ne!(
        created.status,
        StatusCode::FORBIDDEN,
        "member POST /cases must not 403: {}",
        created.body
    );
    assert!(
        created.status == StatusCode::CREATED || created.status == StatusCode::OK,
        "member POST /cases must create (201/200), got {}: {}",
        created.status,
        created.body
    );
    assert_product_body_has_no_secrets(&created, Some(&m_jar), "member POST /cases");
    let case_id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case id: {}", created.body))
        .to_string();

    let a_list = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(a_list.status, StatusCode::OK, "{}", a_list.body);
    let a_cases = a_list.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", a_list.body));
    assert!(
        a_cases.iter().any(|c| {
            c["id"].as_str() == Some(case_id.as_str())
                && c["title"].as_str() == Some(case_title.as_str())
        }),
        "A GET /cases must list member-created {case_id}: {}",
        a_list.body
    );

    let m_invite = call(
        &app,
        "POST",
        &invites_path(&a_ws),
        Some(&m_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(
        m_invite.status,
        StatusCode::FORBIDDEN,
        "member POST /invites must stay 403: {}",
        m_invite.body
    );

    let m_host = call(
        &app,
        "POST",
        &hosts_path(&a_ws),
        Some(&m_jar),
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(
        m_host.status,
        StatusCode::FORBIDDEN,
        "member POST /worker-hosts must stay 403: {}",
        m_host.body
    );

    let b_case_title = format!("b-only-case-{}", Uuid::new_v4());
    let b_case_id = create_open_case(&app, &b_jar, &b_ws, &b_case_title).await;
    let b_ticket_title = format!("b-only-ticket-{}", Uuid::new_v4());
    let b_ticket_key = format!("b-ws-{}/item#1", Uuid::new_v4());
    let b_ws_id = Uuid::parse_str(&b_ws).expect("B workspace id");
    let b_ticket = ticket_repo::upsert_inbox(
        &state.db,
        &UpsertInbox {
            workspace_id: b_ws_id,
            provider: "github".into(),
            external_key: b_ticket_key.clone(),
            title: b_ticket_title.clone(),
            url: None,
            summary: "B-only inbox".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("seed B inbox");
    let b_ticket_id = b_ticket.id.to_string();

    let b_fleet = call(&app, "GET", &fleet(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_fleet.status,
        StatusCode::OK,
        "B GET /fleet: {}",
        b_fleet.body
    );
    let b_project_id = b_fleet.body["project_id"]
        .as_str()
        .unwrap_or_else(|| panic!("B project_id: {}", b_fleet.body))
        .to_string();
    let b_inbox = call(&app, "GET", &inbox_path(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_inbox.status,
        StatusCode::OK,
        "B GET /inbox: {}",
        b_inbox.body
    );
    assert_eq!(
        inbox_ticket(&b_inbox.body, "github", &b_ticket_key)["id"].as_str(),
        Some(b_ticket_id.as_str()),
        "B inbox must list the planted ticket so the leak check is not vacuous: {}",
        b_inbox.body
    );

    let b_secrets = [
        b_ws.as_str(),
        b_case_id.as_str(),
        b_case_title.as_str(),
        b_ticket_id.as_str(),
        b_ticket_key.as_str(),
        b_ticket_title.as_str(),
        b_project_id.as_str(),
    ];
    let foreign = [
        ("/fleet", fleet(&b_ws, "")),
        (
            "/chat/status",
            format!("/api/workspaces/{b_ws}/chat/status"),
        ),
        ("/cases", cases_path(&b_ws, "")),
        ("/inbox", inbox_path(&b_ws, "")),
    ];
    for (label, uri) in &foreign {
        let out = call(&app, "GET", uri, Some(&m_jar), None).await;
        assert_eq!(
            out.status,
            StatusCode::FORBIDDEN,
            "M GET B {label} must be 403 not-a-member: {}",
            out.body
        );
        let err = http_error_text(&out).to_ascii_lowercase();
        assert!(
            err.contains("not a member"),
            "M GET B {label} must say not a member: {}",
            out.body
        );
        assert_ne!(
            out.body.get("ok"),
            Some(&json!(true)),
            "M GET B {label} must not return ok: {}",
            out.body
        );
        for key in ["cases", "tickets", "runs", "sessions", "providers"] {
            if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
                assert!(
                    arr.is_empty(),
                    "M GET B {label} must not include {key}: {}",
                    out.body
                );
            }
        }
        let hay = format!("{} {}", out.raw, out.body);
        for secret in b_secrets {
            assert!(
                !hay.contains(secret),
                "M GET B {label} must not include B's data {secret:?}: {}",
                out.body
            );
        }
    }

    let a_fleet = call(&app, "GET", &fleet(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        a_fleet.status,
        StatusCode::OK,
        "A cookie GET /fleet must stay 200: {}",
        a_fleet.body
    );

    let listed = call(&app, "GET", "/api/workspaces", Some(&m_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .unwrap_or_else(|| panic!("workspaces: {}", listed.body))
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(
        ids.contains(&a_ws.as_str()),
        "M GET /api/workspaces must list A {a_ws}: {ids:?} {}",
        listed.body
    );
}

fn json_object_keys(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            Value::Object(map) => {
                for (k, item) in map {
                    out.push(k.clone());
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut out);
    out
}

fn assert_product_body_has_no_secrets(out: &HttpOut, jar: Option<&CookieJar>, label: &str) {
    const FORBIDDEN_KEYS: &[&str] = &[
        "pair_token",
        "machine_token",
        "access_token",
        "refresh_token",
        "password_hash",
    ];
    for key in json_object_keys(&out.body) {
        assert!(
            !FORBIDDEN_KEYS.contains(&key.as_str()),
            "{label} must not include {key}: {}",
            out.body
        );
    }
    let hay = format!("{} {}", out.raw, out.body);
    if let Some(jar) = jar {
        if let Some(tok) = jar.access() {
            assert!(
                !hay.contains(tok),
                "{label} must not leak access cookie: {}",
                out.body
            );
        }
        if let Some(tok) = jar.refresh() {
            assert!(
                !hay.contains(tok),
                "{label} must not leak refresh cookie: {}",
                out.body
            );
        }
    }
}

// ── 12. leftover: fleet settings + agency-agents personas ───────────────────

#[tokio::test]
async fn fleet_settings_get_patch_and_rejects_grok_store() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let got = call(&app, "GET", &fleet(&ws, "/settings"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["fleet_llm"], "off", "{}", got.body);
    assert_eq!(
        got.body["grok_env"], false,
        "unit/http tests force grok env off"
    );
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

/// Chat tool `configure_fleet` is the model-facing Settings write
/// (off or ollama; concurrent agents 1–`MAX_PROJECT_AGENTS`; Grok env-only).
/// `fleet_settings_get_patch_and_rejects_grok_store` only PATCH
/// `/fleet/settings` `{fleet_llm: grok}` 400 — it never calls `execute_tool`.
/// `patch_fleet_settings_max_agents_zero_or_above_cap_is_400` owns HTTP
/// PATCH 0 / cap+1 400. `set_max_agents_never_lowers` owns launch
/// `GREATEST`. A missing `matches!` or a clamp-and-ok is the product
/// failure — 0 must not become 1, cap+1 must not become the cap.
#[tokio::test]
async fn configure_fleet_rejects_grok_and_range() {
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
    let settings = fleet(&ws, "/settings");
    let cap = fleet_orchestrator::domain::MAX_PROJECT_AGENTS;
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let patched = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 4, "fleet_llm": "ollama" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["max_agents"], 4, "{}", patched.body);
    assert_eq!(patched.body["fleet_llm"], "ollama", "{}", patched.body);

    let snap = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot GET: {}", snap.body);
    let snap_max = snap.body["max_agents"].as_i64().unwrap_or(-1);
    let snap_llm = snap.body["fleet_llm"].as_str().unwrap_or("").to_string();
    let snap_model = snap.body["fleet_llm_model"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(snap_max, 4, "snapshot max_agents: {}", snap.body);
    assert_eq!(snap_llm, "ollama", "snapshot fleet_llm: {}", snap.body);
    assert_ne!(snap_llm, "grok", "snapshot must not already be grok");
    assert_ne!(snap_llm, "openai", "snapshot must not already be openai");

    fn tool_err(content: &Value, action: &fleet_server::chat_tools::ChatAction) -> String {
        content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str())
            .to_string()
    }

    fn claim_hay(content: &Value, action: &fleet_server::chat_tools::ChatAction) -> String {
        let detail = action
            .detail
            .as_ref()
            .map(|d| d.to_string())
            .unwrap_or_default();
        format!("{} {} {} {}", content, action.summary, action.ok, detail).to_ascii_lowercase()
    }

    fn assert_not_stored_backend(
        label: &str,
        forbidden: &str,
        content: &Value,
        action: &fleet_server::chat_tools::ChatAction,
    ) {
        for src in [content, action.detail.as_ref().unwrap_or(content)] {
            let backend = src
                .get("effective_backend")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stored = src.get("fleet_llm").and_then(|v| v.as_str()).unwrap_or("");
            assert_ne!(
                backend, forbidden,
                "{label} body/action must not claim effective_backend={forbidden} as stored: \
                 content={content} action={action:?}"
            );
            assert_ne!(
                stored, forbidden,
                "{label} body/action must not claim fleet_llm={forbidden} as stored: \
                 content={content} action={action:?}"
            );
        }
        let hay = claim_hay(content, action);
        if action.ok {
            assert!(
                !hay.contains(&format!("effective_backend={forbidden}"))
                    && content.get("effective_backend").and_then(|v| v.as_str()) != Some(forbidden),
                "{label} must not claim effective_backend={forbidden} as stored: \
                 content={content} action={action:?}"
            );
        }
    }

    async fn assert_settings_still_snapshot(
        app: &axum::Router,
        jar: &CookieJar,
        state: &AppState,
        ws: &str,
        workspace_id: Uuid,
        snap_max: i64,
        snap_llm: &str,
        snap_model: &str,
        label: &str,
    ) {
        let after = call(app, "GET", &fleet(ws, "/settings"), Some(jar), None).await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "{label} GET /settings: {}",
            after.body
        );
        assert_eq!(
            after.body["max_agents"], snap_max,
            "{label} GET max_agents must stay {snap_max} (clamp would write 1 or the cap): {}",
            after.body
        );
        assert_eq!(
            after.body["fleet_llm"], snap_llm,
            "{label} GET fleet_llm must stay {snap_llm:?} (not grok/openai; store remap would write off): {}",
            after.body
        );
        assert_eq!(
            after.body["fleet_llm_model"], snap_model,
            "{label} GET fleet_llm_model must stay {snap_model:?}: {}",
            after.body
        );
        assert_ne!(
            after.body["fleet_llm"], "grok",
            "{label} GET must not persist grok: {}",
            after.body
        );
        assert_ne!(
            after.body["fleet_llm"], "openai",
            "{label} GET must not persist openai: {}",
            after.body
        );
        assert_ne!(
            after.body["effective_backend"], "grok",
            "{label} GET must not claim effective_backend=grok as stored: {}",
            after.body
        );
        let cfg = state
            .orch
            .store
            .get_project_config(workspace_id)
            .await
            .expect("project config after rejected configure_fleet");
        assert_eq!(
            i64::from(cfg.max_agents),
            snap_max,
            "{label} store max_agents must stay {snap_max}"
        );
        assert_eq!(
            cfg.fleet_llm, snap_llm,
            "{label} store fleet_llm must stay {snap_llm:?}"
        );
        assert_eq!(
            cfg.fleet_llm_model, snap_model,
            "{label} store fleet_llm_model must stay {snap_model:?}"
        );
    }

    for raw in ["grok", "openai"] {
        let (content, action) = fleet_server::chat_tools::execute_tool(
            &state,
            workspace_id,
            user_id,
            "configure_fleet",
            &json!({ "fleet_llm": raw }),
            &llm,
        )
        .await;
        assert!(
            !action.ok,
            "configure_fleet fleet_llm={raw} must fail closed (ok:false or Err/400), \
             not persist: action={action:?} content={content}"
        );
        assert_eq!(
            action.tool, "configure_fleet",
            "configure_fleet fleet_llm={raw} action.tool: {action:?}"
        );
        let err = tool_err(&content, &action);
        let hay = format!("{err} {content}").to_ascii_lowercase();
        assert!(
            hay.contains("grok")
                || hay.contains("env-only")
                || hay.contains("off")
                || hay.contains("ollama"),
            "configure_fleet fleet_llm={raw} reject must name grok/env-only or off or ollama: \
             err={err:?} content={content} action={action:?}"
        );
        assert_not_stored_backend(
            &format!("configure_fleet fleet_llm={raw}"),
            "grok",
            &content,
            &action,
        );
        assert_not_stored_backend(
            &format!("configure_fleet fleet_llm={raw}"),
            "openai",
            &content,
            &action,
        );
        assert_settings_still_snapshot(
            &app,
            &jar,
            &state,
            &ws,
            workspace_id,
            snap_max,
            &snap_llm,
            &snap_model,
            &format!("configure_fleet fleet_llm={raw}"),
        )
        .await;
    }

    for raw in [0, cap + 1] {
        let (content, action) = fleet_server::chat_tools::execute_tool(
            &state,
            workspace_id,
            user_id,
            "configure_fleet",
            &json!({ "max_agents": raw }),
            &llm,
        )
        .await;
        assert!(
            !action.ok,
            "configure_fleet max_agents={raw} must fail closed (not clamp-and-ok): \
             action={action:?} content={content}"
        );
        assert_eq!(
            action.tool, "configure_fleet",
            "configure_fleet max_agents={raw} action.tool: {action:?}"
        );
        let err = tool_err(&content, &action);
        let hay = format!("{err} {content}").to_ascii_lowercase();
        assert!(
            hay.contains("max_agents"),
            "configure_fleet max_agents={raw} reject must name the field (same 1–cap \
             contract as HTTP PATCH): err={err:?} content={content} action={action:?}"
        );
        let clamped = if raw < 1 { 1 } else { cap };
        if let Some(got) = content.get("max_agents").and_then(|v| v.as_i64()) {
            assert_ne!(
                got,
                i64::from(clamped),
                "configure_fleet max_agents={raw} must not present clamped {clamped} as applied: \
                 content={content} action={action:?}"
            );
        }
        assert_settings_still_snapshot(
            &app,
            &jar,
            &state,
            &ws,
            workspace_id,
            snap_max,
            &snap_llm,
            &snap_model,
            &format!("configure_fleet max_agents={raw}"),
        )
        .await;
    }

    let (control_content, control_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "configure_fleet",
        &json!({
            "fleet_llm": "ollama",
            "fleet_llm_model": "llama3.2"
        }),
        &llm,
    )
    .await;
    assert!(
        control_action.ok,
        "control configure_fleet ollama/llama3.2 must succeed so this is not a tautology \
         that every configure_fleet fails: action={control_action:?} content={control_content}"
    );
    let after_control = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(
        after_control.status,
        StatusCode::OK,
        "control GET: {}",
        after_control.body
    );
    assert_eq!(
        after_control.body["fleet_llm"], "ollama",
        "control configure_fleet must persist ollama: {}",
        after_control.body
    );
    assert_eq!(
        after_control.body["fleet_llm_model"], "llama3.2",
        "control configure_fleet must persist llama3.2: {}",
        after_control.body
    );
    assert_eq!(
        after_control.body["max_agents"], snap_max,
        "control must not change snapshotted max_agents: {}",
        after_control.body
    );

    let grok = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "fleet_llm": "grok" })),
    )
    .await;
    assert_eq!(
        grok.status,
        StatusCode::BAD_REQUEST,
        "HTTP PATCH {{fleet_llm: grok}} must still 400 (do not weaken \
         fleet_settings_get_patch_and_rejects_grok_store): {}",
        grok.body
    );
    let after_http = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after_http.status, StatusCode::OK, "{}", after_http.body);
    assert_eq!(
        after_http.body["fleet_llm"], "ollama",
        "HTTP PATCH grok must leave stored fleet_llm: {}",
        after_http.body
    );
}

/// Settings PATCH must 400 on 0 / above cap — store clamp is not the HTTP gate.
/// `update_fleet_settings` would turn 0 into 1 (and cap+1 into the cap) and
/// look applied. Other-workspace cookie is 403, not a range 400.
/// Distinct from host `max_concurrent` (clamp-or-400 1–256) and chat
/// `set_max_agents` (GREATEST raise-only — a valid lower PATCH must apply).
fn assert_settings_max_agents_range_400(out: &HttpOut, raw: i32) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "max_agents={raw} must 400, not clamp-and-200: {}",
        out.body
    );
    let err = out.body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("max_agents"),
        "400 body must name the field: {}",
        out.body
    );
    assert!(
        out.body.get("max_agents").is_none(),
        "400 must not return applied settings (store clamp would look applied): {}",
        out.body
    );
}

#[tokio::test]
async fn patch_fleet_settings_max_agents_zero_or_above_cap_is_400() {
    let app = app().await;
    let (_a, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b, b_jar, _) = signup_and_login(&app).await;
    let settings = fleet(&a_ws, "/settings");
    let cap = fleet_orchestrator::domain::MAX_PROJECT_AGENTS;
    let prior = 5i32;

    let set_prior = call(
        &app,
        "PATCH",
        &settings,
        Some(&a_jar),
        Some(json!({ "max_agents": prior })),
    )
    .await;
    assert_eq!(set_prior.status, StatusCode::OK, "{}", set_prior.body);
    assert_eq!(
        set_prior.body["max_agents"], prior,
        "in-range PATCH must store the requested value: {}",
        set_prior.body
    );
    assert_eq!(
        set_prior.body["max_agents_cap"].as_i64().unwrap_or(0),
        i64::from(cap),
        "settings must expose MAX_PROJECT_AGENTS as max_agents_cap: {}",
        set_prior.body
    );

    for raw in [0, -1, cap + 1] {
        let rejected = call(
            &app,
            "PATCH",
            &settings,
            Some(&a_jar),
            Some(json!({ "max_agents": raw })),
        )
        .await;
        assert_settings_max_agents_range_400(&rejected, raw);

        let after = call(&app, "GET", &settings, Some(&a_jar), None).await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        assert_eq!(
            after.body["max_agents"],
            prior,
            "failed PATCH {raw} must leave stored max_agents unchanged \
             (store clamp would write {}): {}",
            if raw < 1 { 1 } else { cap },
            after.body
        );
    }

    let other_valid = call(
        &app,
        "PATCH",
        &settings,
        Some(&b_jar),
        Some(json!({ "max_agents": 7 })),
    )
    .await;
    assert_eq!(
        other_valid.status,
        StatusCode::FORBIDDEN,
        "other-workspace cookie must 403 (not apply 7): {}",
        other_valid.body
    );

    let other_zero = call(
        &app,
        "PATCH",
        &settings,
        Some(&b_jar),
        Some(json!({ "max_agents": 0 })),
    )
    .await;
    assert_eq!(
        other_zero.status,
        StatusCode::FORBIDDEN,
        "other-workspace cookie must 403 before range (not 400): {}",
        other_zero.body
    );

    let still = call(&app, "GET", &settings, Some(&a_jar), None).await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(
        still.body["max_agents"], prior,
        "other-workspace PATCH must not mutate stored max_agents: {}",
        still.body
    );

    // Settings is a set. Chat `set_max_agents` is GREATEST and would keep 5.
    let lowered = call(
        &app,
        "PATCH",
        &settings,
        Some(&a_jar),
        Some(json!({ "max_agents": 2 })),
    )
    .await;
    assert_eq!(
        lowered.status,
        StatusCode::OK,
        "in-range lower PATCH must apply (not GREATEST raise-only): {}",
        lowered.body
    );
    assert_eq!(
        lowered.body["max_agents"], 2,
        "valid lower max_agents must store, not stay at {prior}: {}",
        lowered.body
    );
    let after_lower = call(&app, "GET", &settings, Some(&a_jar), None).await;
    assert_eq!(
        after_lower.body["max_agents"], 2,
        "GET after lower PATCH: {}",
        after_lower.body
    );

    for bound in [1, cap] {
        let ok = call(
            &app,
            "PATCH",
            &settings,
            Some(&a_jar),
            Some(json!({ "max_agents": bound })),
        )
        .await;
        assert_eq!(
            ok.status,
            StatusCode::OK,
            "in-range bound {bound} must 200: {}",
            ok.body
        );
        assert_eq!(
            ok.body["max_agents"], bound,
            "in-range bound {bound} must store exactly (not clamp): {}",
            ok.body
        );
    }
}

/// Docs §7.1: `launch_agent_task` → `ensure_project_capacity` →
/// `set_max_agents` is GREATEST (raise-only). Settings PATCH 32 must
/// stick after a later launch with default `FLEET_MAX_AGENTS` (16) or
/// `args.max_agents=1`. `SET max_agents=$2` would drop concurrency and
/// look like Settings did not stick. Control: Settings 2 + launch with
/// a higher max may raise. Distinct from
/// `patch_fleet_settings_max_agents_zero_or_above_cap_is_400` (HTTP 0 /
/// cap+1 is 400) and tick extra-ready stay-ready (spawn slots).
#[tokio::test]
async fn set_max_agents_never_lowers() {
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
    let settings = fleet(&ws, "/settings");
    let chat_cap = fleet_server::chat_tools::chat_max_agents();
    assert!(
        chat_cap >= 8,
        "FLEET_MAX_AGENTS default is 16; raise control needs room above Settings 2 (got {chat_cap})"
    );
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let set_32 = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 32 })),
    )
    .await;
    assert_eq!(set_32.status, StatusCode::OK, "{}", set_32.body);
    assert_eq!(
        set_32.body["max_agents"], 32,
        "Settings PATCH 32 must store: {}",
        set_32.body
    );

    let (default_content, default_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": format!("default-cap-{}", Uuid::new_v4()),
            "spec": "Launch with omitted max_agents (FLEET_MAX_AGENTS default).",
            "worker_profile": "coding"
        }),
        &llm,
    )
    .await;
    assert!(
        default_action.ok,
        "launch_agent_task (default max_agents) must succeed so set_max_agents ran: \
         action={default_action:?} content={default_content}"
    );
    let after_default = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(
        after_default.status,
        StatusCode::OK,
        "{}",
        after_default.body
    );
    let expected_after_default = 32i32.max(chat_cap);
    assert_eq!(
        after_default.body["max_agents"], expected_after_default,
        "Settings 32 + omitted max_agents (FLEET_MAX_AGENTS={chat_cap}) must GREATEST, \
         not SET to {chat_cap}: {}",
        after_default.body
    );
    let store_after_default = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .expect("project config after default launch");
    assert_eq!(
        store_after_default.max_agents, expected_after_default,
        "store max_agents after default launch"
    );

    let (one_content, one_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": format!("one-agent-{}", Uuid::new_v4()),
            "spec": "Launch with args.max_agents=1 must not lower Settings.",
            "worker_profile": "coding",
            "max_agents": 1
        }),
        &llm,
    )
    .await;
    assert!(
        one_action.ok,
        "launch_agent_task (max_agents=1) must succeed so set_max_agents ran: \
         action={one_action:?} content={one_content}"
    );
    let after_one = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after_one.status, StatusCode::OK, "{}", after_one.body);
    assert_eq!(
        after_one.body["max_agents"], expected_after_default,
        "args.max_agents=1 must not lower stored max_agents (SET max_agents=$2 would write 1): {}",
        after_one.body
    );
    let store_after_one = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .expect("project config after max_agents=1 launch");
    assert_eq!(
        store_after_one.max_agents, expected_after_default,
        "store max_agents after max_agents=1 launch"
    );

    let set_2 = call(
        &app,
        "PATCH",
        &settings,
        Some(&jar),
        Some(json!({ "max_agents": 2 })),
    )
    .await;
    assert_eq!(
        set_2.status,
        StatusCode::OK,
        "Settings is a set and may lower: {}",
        set_2.body
    );
    assert_eq!(set_2.body["max_agents"], 2, "{}", set_2.body);

    let raise_to = 8i32.min(chat_cap);
    let (raise_content, raise_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_task",
        &json!({
            "title": format!("raise-cap-{}", Uuid::new_v4()),
            "spec": "Launch with a higher max_agents may raise Settings 2.",
            "worker_profile": "coding",
            "max_agents": raise_to
        }),
        &llm,
    )
    .await;
    assert!(
        raise_action.ok,
        "launch_agent_task (max_agents={raise_to}) must succeed so GREATEST can raise: \
         action={raise_action:?} content={raise_content}"
    );
    let after_raise = call(&app, "GET", &settings, Some(&jar), None).await;
    assert_eq!(after_raise.status, StatusCode::OK, "{}", after_raise.body);
    assert_eq!(
        after_raise.body["max_agents"], raise_to,
        "Settings 2 + launch max_agents={raise_to} must raise (GREATEST), not stay at 2: {}",
        after_raise.body
    );
    let store_after_raise = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .expect("project config after raise launch");
    assert_eq!(
        store_after_raise.max_agents, raise_to,
        "store max_agents after raise launch"
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
        personas
            .iter()
            .any(|p| p["slug"].as_str().unwrap_or("").contains("backend")
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
        runner_id: None,
        effort: None,
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
    assert!(listed["count"].as_u64().unwrap_or(0) >= 1, "{listed}");

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

/// Docs §7.1: `launch_agent_plan` max 1000 tasks. 1001 unique ids, a
/// cycle under the cap, and an unknown dep fail closed on `execute_tool`
/// (ok:false, no run_id) and insert no accepted plan/run. A legal 1–2
/// task plan still launches so this is not “every plan fails”. Distinct
/// from the 200-char title cap. Do not launch 1000 tasks here.
#[tokio::test]
async fn chat_plan_execute_tool_rejects_over_max_tasks_and_inserts_no_run() {
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
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);

    let snap_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "snapshot tasks: {}",
        snap_tasks.body
    );
    let snap_task_ids = fleet_task_ids(&snap_tasks.body);

    fn tool_err(content: &Value, action: &fleet_server::chat_tools::ChatAction) -> String {
        content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str())
            .to_string()
    }

    fn assert_plan_rejected(
        label: &str,
        action: &fleet_server::chat_tools::ChatAction,
        content: &Value,
    ) {
        assert!(
            !action.ok,
            "{label} must fail closed (ok:false): {action:?} {content}"
        );
        assert_eq!(
            action.tool, "launch_agent_plan",
            "{label} action.tool: {action:?}"
        );
        assert_eq!(
            action.run_id, None,
            "{label} must not return a run_id: {action:?}"
        );
        assert!(
            content.get("run_id").and_then(|v| v.as_str()).is_none(),
            "{label} body must not include a run_id: {content}"
        );
    }

    async fn assert_runs_unchanged(
        app: &axum::Router,
        jar: &CookieJar,
        ws: &str,
        snap_ids: &[String],
        label: &str,
    ) {
        let runs = call(app, "GET", &fleet(ws, "/runs"), Some(jar), None).await;
        assert_eq!(runs.status, StatusCode::OK, "{label} runs: {}", runs.body);
        let after_ids = fleet_run_ids(&runs.body);
        assert_eq!(
            after_ids, snap_ids,
            "{label} GET /fleet/runs must still be the snapshot: snap={snap_ids:?} after={after_ids:?} body={}",
            runs.body
        );
    }

    let over_title = format!("chat-plan-over-{}", Uuid::new_v4());
    let over_tasks: Vec<Value> = (0..=1000)
        .map(|i| json!({ "id": format!("t{i}"), "title": format!("Task {i}") }))
        .collect();
    assert_eq!(over_tasks.len(), 1001);
    let over_ids: std::collections::HashSet<&str> = over_tasks
        .iter()
        .map(|t| t["id"].as_str().expect("task id"))
        .collect();
    assert_eq!(over_ids.len(), 1001, "fixture must be 1001 unique ids");

    let (over_content, over_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "title": over_title,
            "tasks": over_tasks,
        }),
        &llm,
    )
    .await;
    assert_plan_rejected("1001 unique tasks", &over_action, &over_content);
    let over_err = tool_err(&over_content, &over_action);
    let over_lower = over_err.to_ascii_lowercase();
    assert!(
        (over_lower.contains("too many") || over_lower.contains("exceed"))
            && (over_lower.contains("max") || over_lower.contains("1000")),
        "1001 must fail the task cap (not a missing-field error): {over_err} {over_content}"
    );
    assert!(
        !over_lower.contains("missing field")
            && !over_lower.contains("invalid launch_agent_plan args"),
        "1001 must fail the task cap, not a field check: {over_err} {over_content}"
    );
    assert_runs_unchanged(&app, &jar, &ws, &snap_ids, "1001 unique tasks").await;
    let after_over_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        after_over_tasks.status,
        StatusCode::OK,
        "1001 unique tasks GET /fleet/tasks: {}",
        after_over_tasks.body
    );
    let after_over_task_ids = fleet_task_ids(&after_over_tasks.body);
    assert_eq!(
        after_over_task_ids,
        snap_task_ids,
        "1001 unique tasks must not grow 1001 board tasks: snap={} after={} body={}",
        snap_task_ids.len(),
        after_over_task_ids.len(),
        after_over_tasks.body
    );
    assert_eq!(
        after_over_task_ids
            .len()
            .saturating_sub(snap_task_ids.len()),
        0,
        "GET /fleet/tasks must not grow 1001 new board tasks: snap={} after={} body={}",
        snap_task_ids.len(),
        after_over_task_ids.len(),
        after_over_tasks.body
    );

    let cycle_title = format!("chat-plan-cycle-{}", Uuid::new_v4());
    let (cycle_content, cycle_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "title": cycle_title,
            "tasks": [
                { "id": "a", "title": "A", "depends_on": ["b"] },
                { "id": "b", "title": "B", "depends_on": ["a"] }
            ]
        }),
        &llm,
    )
    .await;
    assert_plan_rejected("cycle under the cap", &cycle_action, &cycle_content);
    let cycle_err = tool_err(&cycle_content, &cycle_action);
    assert!(
        cycle_err.to_ascii_lowercase().contains("cycle"),
        "cycle under the cap must mention cycle: {cycle_err} {cycle_content}"
    );
    assert_runs_unchanged(&app, &jar, &ws, &snap_ids, "cycle under the cap").await;

    let unknown_title = format!("chat-plan-unknown-{}", Uuid::new_v4());
    let (unknown_content, unknown_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "title": unknown_title,
            "tasks": [
                { "id": "a", "title": "A", "depends_on": ["ghost"] }
            ]
        }),
        &llm,
    )
    .await;
    assert_plan_rejected(
        "unknown dep under the cap",
        &unknown_action,
        &unknown_content,
    );
    assert_runs_unchanged(&app, &jar, &ws, &snap_ids, "unknown dep under the cap").await;

    let control_title = format!("chat-plan-ok-{}", Uuid::new_v4());
    let (control_content, control_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "title": control_title,
            "tasks": [
                { "id": "impl", "title": "Implement", "spec": "do it", "worker_profile": "coding" },
                { "id": "rev", "title": "Review", "spec": "review it", "worker_profile": "reviewer", "depends_on": ["impl"] }
            ]
        }),
        &llm,
    )
    .await;
    assert!(
        control_action.ok,
        "legal 1–2 task plan must still succeed: {control_action:?} {control_content}"
    );
    let control_run = control_action
        .run_id
        .as_deref()
        .unwrap_or_else(|| panic!("legal plan must return run_id: {control_action:?}"));
    assert!(
        Uuid::parse_str(control_run).is_ok(),
        "legal plan run_id must be a uuid: {control_run:?} {control_action:?}"
    );
    let after_control = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_control.status,
        StatusCode::OK,
        "runs after legal plan: {}",
        after_control.body
    );
    let after_control_ids = fleet_run_ids(&after_control.body);
    assert!(
        after_control_ids.iter().any(|id| id == control_run),
        "GET /fleet/runs must list legal-plan run {control_run}: {}",
        after_control.body
    );
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

    let overview = call(&app, "GET", &fleet(&ws, "/overview"), Some(&jar), None).await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    assert!(
        overview.body["total_runs"].as_u64().unwrap_or(0) >= 1,
        "{}",
        overview.body
    );
    assert_eq!(overview.body["chat"].as_array().map(|a| a.len()), Some(1));
}

/// `send_team_message` / `read_team_chat` reject a `run_id` that is not
/// this workspace's run. Distinct from payload `workspace_id` on other
/// tools (`require_session_workspace`). Filter `-- team_chat`.
#[tokio::test]
async fn chat_tools_team_chat_run_scoped_to_workspace() {
    let (app, state) = app_with_state().await;
    let (_email, jar, a_ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    let user_id = Uuid::parse_str(me.body["user"]["id"].as_str().unwrap()).unwrap();
    let a_ws_id = Uuid::parse_str(&a_ws).unwrap();
    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("team-chat-b-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "POST /api/workspaces B: {}",
        b_created.body
    );
    let b_ws = b_created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.id: {}", b_created.body))
        .to_string();
    let b_ws_id = Uuid::parse_str(&b_ws).expect("b_ws");
    assert_ne!(a_ws_id, b_ws_id, "A and B must be distinct workspaces");

    let (a_launch, a_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "t1", "title": "API-A", "spec": "build A", "worker_profile": "coding" }
            ]
        }),
        &llm,
    )
    .await;
    assert!(a_action.ok, "{a_action:?} {a_launch}");
    let a_run = a_action.run_id.as_deref().expect("A run_id");
    let a_run_id = Uuid::parse_str(a_run).expect("A run uuid");

    let (b_launch, b_action) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "launch_agent_plan",
        &json!({
            "tasks": [
                { "id": "t1", "title": "API-B", "spec": "build B", "worker_profile": "coding" }
            ]
        }),
        &llm,
    )
    .await;
    assert!(b_action.ok, "{b_action:?} {b_launch}");
    let b_run = b_action.run_id.as_deref().expect("B run_id").to_string();
    assert_ne!(a_run, b_run, "A and B must have distinct runs");

    let a_secret = format!("port-A-{}", Uuid::new_v4());
    let (a_sent, a_send) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": a_run, "to": "t1", "body": a_secret }),
        &llm,
    )
    .await;
    assert!(a_send.ok, "control send on A's run: {a_send:?} {a_sent}");
    assert_eq!(a_sent["run_id"].as_str(), Some(a_run), "{a_sent}");

    let data_dir = state.orch.tick.data_dir();
    let a_chat = fleet_orchestrator::sandbox::team_chat::run_dir(data_dir, a_ws_id, a_run_id)
        .join("chat.jsonl");
    assert!(
        a_chat.is_file(),
        "control must write A's chat.jsonl: {a_chat:?}"
    );

    fn assert_team_chat_rejected(
        tool: &str,
        action: &fleet_server::chat_tools::ChatAction,
        body: &Value,
    ) {
        assert!(
            !action.ok,
            "{tool} with a run_id that is not this workspace's run must be ok:false: {action:?} {body}"
        );
        assert_eq!(
            action.run_id, None,
            "{tool} must not echo a foreign/unknown run as success: {action:?}"
        );
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(action.summary.as_str());
        let lower = err.to_ascii_lowercase();
        assert!(
            lower.contains("not found")
                || lower.contains("run_id must be a uuid")
                || (lower.contains("run")
                    && (lower.contains("unknown") || lower.contains("invalid"))),
            "{tool} must fail closed 404/400, not succeed: {err} {body}"
        );
    }

    let ghost_body = format!("ghost-from-B-{}", Uuid::new_v4());
    let (cross_sent, cross_send) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": a_run, "to": "t1", "body": ghost_body }),
        &llm,
    )
    .await;
    assert_team_chat_rejected("send_team_message", &cross_send, &cross_sent);

    let (cross_read, cross_read_action) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "read_team_chat",
        &json!({ "run_id": a_run }),
        &llm,
    )
    .await;
    assert_team_chat_rejected("read_team_chat", &cross_read_action, &cross_read);
    let cross_s = format!("{cross_read}{cross_sent}");
    assert!(
        !cross_s.contains(&a_secret),
        "B must not see A's team chat via A's run_id: {cross_read}"
    );

    let random_run = Uuid::now_v7();
    let (rand_sent, rand_send) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "send_team_message",
        &json!({
            "run_id": random_run.to_string(),
            "to": "*",
            "body": format!("ghost-random-{}", Uuid::new_v4())
        }),
        &llm,
    )
    .await;
    assert_team_chat_rejected("send_team_message", &rand_send, &rand_sent);

    let (rand_read, rand_read_action) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "read_team_chat",
        &json!({ "run_id": random_run.to_string() }),
        &llm,
    )
    .await;
    assert_team_chat_rejected("read_team_chat", &rand_read_action, &rand_read);

    let (bad_sent, bad_send) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "send_team_message",
        &json!({ "run_id": "not-a-uuid", "body": "nope" }),
        &llm,
    )
    .await;
    assert!(
        !bad_send.ok,
        "malformed run_id must be ok:false: {bad_send:?} {bad_sent}"
    );
    let bad_err = bad_sent
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or(bad_send.summary.as_str())
        .to_ascii_lowercase();
    assert!(
        bad_err.contains("uuid") || bad_err.contains("run_id"),
        "malformed run_id is 400: {bad_err} {bad_sent}"
    );

    let ghost_a_under_b =
        fleet_orchestrator::sandbox::team_chat::run_dir(data_dir, b_ws_id, a_run_id);
    assert!(
        !ghost_a_under_b.join("chat.jsonl").exists(),
        "send_team_message must not write a ghost chat.jsonl under B for A's run: {ghost_a_under_b:?}"
    );
    assert!(
        !ghost_a_under_b.join("TEAM_CHAT.md").exists(),
        "must not write TEAM_CHAT.md under B for A's run: {ghost_a_under_b:?}"
    );
    let ghost_rand_under_b =
        fleet_orchestrator::sandbox::team_chat::run_dir(data_dir, b_ws_id, random_run);
    assert!(
        !ghost_rand_under_b.exists() || !ghost_rand_under_b.join("chat.jsonl").exists(),
        "random run_id must not create chat.jsonl under B: {ghost_rand_under_b:?}"
    );

    let b_note = format!("only-on-B-{}", Uuid::new_v4());
    let (omitted_sent, omitted_send) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "send_team_message",
        &json!({ "to": "*", "body": b_note }),
        &llm,
    )
    .await;
    assert!(
        omitted_send.ok,
        "omitted run_id must resolve to B's open run: {omitted_send:?} {omitted_sent}"
    );
    assert_eq!(
        omitted_sent["run_id"].as_str(),
        Some(b_run.as_str()),
        "omitted run_id must stay in this workspace: {omitted_sent}"
    );

    let (omitted_read, omitted_read_action) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "read_team_chat",
        &json!({}),
        &llm,
    )
    .await;
    assert!(
        omitted_read_action.ok,
        "omitted run_id read must use B's run: {omitted_read_action:?} {omitted_read}"
    );
    assert_eq!(
        omitted_read["run_id"].as_str(),
        Some(b_run.as_str()),
        "{omitted_read}"
    );
    let transcript = omitted_read["transcript"].as_str().unwrap_or("");
    assert!(
        transcript.contains(&b_note),
        "B's omitted-run read must include B's message: {omitted_read}"
    );
    assert!(
        !transcript.contains(&a_secret) && !transcript.contains(&ghost_body),
        "B's omitted-run read must not include A's chat: {omitted_read}"
    );

    let (a_reread, a_reread_action) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "read_team_chat",
        &json!({ "run_id": a_run }),
        &llm,
    )
    .await;
    assert!(
        a_reread_action.ok,
        "control reread A: {a_reread_action:?} {a_reread}"
    );
    let a_transcript = a_reread["transcript"].as_str().unwrap_or("");
    assert!(
        a_transcript.contains(&a_secret),
        "A's chat must still have the control message: {a_reread}"
    );
    assert!(
        !a_transcript.contains(&ghost_body) && !a_transcript.contains(&b_note),
        "rejected / B messages must not land on A's chat: {a_reread}"
    );
}

#[tokio::test]
async fn chat_tools_ticket_inbox_is_available() {
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

    let (listed, action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "list_tickets",
        &json!({}),
        &llm,
    )
    .await;
    assert!(action.ok, "list_tickets {action:?} {listed}");

    let inbox = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/inbox"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
}

#[tokio::test]
async fn cases_create_list_observe_use_tool_and_cancel() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/cases"),
        Some(&jar),
        Some(json!({
            "title": "Slack feature",
            "policy_md": "Read #eng, implement, PR, Notion page.",
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let case_id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", created.body))
        .to_string();
    assert_eq!(created.body["status"], "open");
    assert_eq!(created.body["stage"], "start");

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/cases"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(
        list.body["cases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == case_id),
        "{}",
        list.body
    );

    let observed = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/cases/{case_id}/observe"),
        Some(&jar),
        Some(json!({ "note": "eng wants dark mode", "wake": false })),
    )
    .await;
    assert_eq!(observed.status, StatusCode::OK, "{}", observed.body);

    let decided = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/cases/{case_id}/decide"),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "use_tool",
                "tool": {
                    "provider": "slack",
                    "operation": "get_channel_history",
                    "args": { "channel": "Cdoesnotexist" }
                },
                "wait_seconds": 30,
                "reason": "read eng"
            }
        })),
    )
    .await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    let err = decided.body["last_error"].as_str().unwrap_or("");
    assert!(
        !err.contains("not wired"),
        "use_tool should invoke the proxy: {err}"
    );
    assert!(
        decided.body["context"]["last_tool_result"].is_object(),
        "{}",
        decided.body
    );

    let got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/cases/{case_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    let kinds: Vec<&str> = got.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"created"), "{kinds:?}");
    assert!(kinds.contains(&"observed"), "{kinds:?}");
    assert!(
        kinds.iter().any(|k| *k == "tool_ok" || *k == "tool_error"),
        "{kinds:?}"
    );

    let cancelled = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/cases/{case_id}/cancel"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["status"], "cancelled");
}

/// POST /cases empty / whitespace / >300 title is 400 and inserts no row.
/// `create_case` trims then rejects `title` outside 1–300. Observe-note and
/// launch_agent_* 200-char titles are other cards. A handler that stores "" /
/// 301 x's (or clamps-and-200s) is the failure.
#[tokio::test]
async fn cases_title_empty_or_over_300_is_400_and_inserts_no_row() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let snap = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "pre-call GET /cases: {}",
        snap.body
    );
    let snap_rows = listed_case_id_titles(&snap.body);
    let snap_count = snap.body["count"]
        .as_u64()
        .unwrap_or_else(|| snap_rows.len() as u64);
    assert_eq!(
        snap_count as usize,
        snap_rows.len(),
        "snapshot count must match cases array: {}",
        snap.body
    );

    let over = "x".repeat(301);
    assert_eq!(over.len(), 301);
    let bads: &[(&str, &str)] = &[
        ("", "empty"),
        ("   ", "whitespace"),
        (over.as_str(), "301-char"),
    ];

    for (title, label) in bads {
        let out = call(
            &app,
            "POST",
            &cases_path(&ws, ""),
            Some(&jar),
            Some(json!({ "title": title })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{label} title {title:?} must not 500: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::OK,
            "{label} title {title:?} must not 200 (inserts or clamps-and-200s): {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::CREATED,
            "{label} title {title:?} must not 201 (inserts or clamps-and-200s): {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "{label} title {title:?} must be 400: {}",
            out.body
        );
        let err = http_error_text(&out).to_ascii_lowercase();
        assert!(
            err.contains("title")
                && (err.contains("1–300") || err.contains("1-300") || err.contains("300")),
            "{label} 400 must be title must be 1–300 chars, got {:?}: {}",
            out.body.get("error"),
            out.body
        );
        assert!(
            out.body.get("id").and_then(|v| v.as_str()).is_none(),
            "{label} 400 must not return a case id: {}",
            out.body
        );
        if !title.is_empty() {
            assert!(
                !out.raw.contains(title),
                "{label} 400 body must not echo the rejected title: {}",
                out.status
            );
        }

        let after = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "GET /cases after {label} reject: {}",
            after.body
        );
        let after_rows = listed_case_id_titles(&after.body);
        let after_count = after.body["count"]
            .as_u64()
            .unwrap_or_else(|| after_rows.len() as u64);
        assert_eq!(
            after_count, snap_count,
            "rejected {label} title must not change GET /cases count: snap={snap_count} after={after_count} body={}",
            after.body
        );
        assert_eq!(
            after_rows, snap_rows,
            "GET /cases after {label} reject must match the snapshot (no extra row): snap={snap_rows:?} after={after_rows:?} body={}",
            after.body
        );
        assert!(
            after_rows
                .iter()
                .all(|(_, t)| t != title && t != &title.trim()),
            "rejected {label} title {title:?} must not appear on GET /cases: {}",
            after.body
        );
        if !title.trim().is_empty() {
            assert!(
                !after.raw.contains(title)
                    && after.body["cases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .all(|c| c["title"].as_str() != Some(title)),
                "GET /cases after {label} reject must not contain the rejected title"
            );
        }
    }

    // list-cases-list-create-other-ws-403 owns the full list/create IDOR.
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(ws, b_ws, "A and B must have distinct workspaces");
    let stolen = format!("cases-title-stolen-{}", Uuid::new_v4());
    let create_as_b = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&b_jar),
        Some(json!({ "title": stolen })),
    )
    .await;
    assert_eq!(
        create_as_b.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A's POST /cases must be 403 not-a-member: {}",
        create_as_b.body
    );
    let err_b = http_error_text(&create_as_b).to_ascii_lowercase();
    assert!(
        err_b.contains("not a member"),
        "other-ws POST must say not-a-member: {}",
        create_as_b.body
    );
    assert!(
        create_as_b
            .body
            .get("id")
            .and_then(|v| v.as_str())
            .is_none(),
        "other-ws 403 must not return a case id: {}",
        create_as_b.body
    );
    let after_b = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(after_b.status, StatusCode::OK, "{}", after_b.body);
    let after_b_rows = listed_case_id_titles(&after_b.body);
    let after_b_count = after_b.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_b_rows.len() as u64);
    assert_eq!(
        after_b_count, snap_count,
        "other-ws POST must not insert on A: snap={snap_count} after={after_b_count} body={}",
        after_b.body
    );
    assert!(
        after_b_rows.iter().all(|(_, title)| title != &stolen) && !after_b.raw.contains(&stolen),
        "other-ws POST must not insert stolen title on A: {}",
        after_b.body
    );

    let one = "Z";
    let one_out = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({ "title": one })),
    )
    .await;
    assert_eq!(
        one_out.status,
        StatusCode::OK,
        "control 1-char title must 200 (not a tautology that every create 400s): {}",
        one_out.body
    );
    assert_eq!(
        one_out.body["title"].as_str(),
        Some(one),
        "control POST must persist the 1-char title: {}",
        one_out.body
    );
    let after_one = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        after_one.status,
        StatusCode::OK,
        "GET /cases after 1-char create: {}",
        after_one.body
    );
    assert!(
        listed_case_id_titles(&after_one.body)
            .iter()
            .any(|(_, title)| title == one),
        "GET /cases must list the 1-char title: {}",
        after_one.body
    );

    let prefix = format!("T300-{}-", Uuid::new_v4());
    assert!(prefix.len() < 300, "300-char unique prefix too long");
    let at_cap = format!("{prefix}{}", "x".repeat(300 - prefix.len()));
    assert_eq!(at_cap.len(), 300, "control title must be exactly 300 chars");
    let cap_out = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({ "title": at_cap })),
    )
    .await;
    assert_eq!(
        cap_out.status,
        StatusCode::OK,
        "control 300-char title must 200 (not a tautology that every create 400s): {}",
        cap_out.body
    );
    assert_eq!(
        cap_out.body["title"].as_str(),
        Some(at_cap.as_str()),
        "control POST must persist the exact 300-char title (not clamp-and-200): {}",
        cap_out.body
    );
    let after_cap = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        after_cap.status,
        StatusCode::OK,
        "GET /cases after 300-char create: {}",
        after_cap.body
    );
    assert!(
        listed_case_id_titles(&after_cap.body)
            .iter()
            .any(|(_, title)| title == &at_cap),
        "GET /cases must list the 300-char title: {}",
        after_cap.body
    );
}

/// POST /cases `policy_md` over 32k is 400 and inserts no row.
/// Same 32k family as launch spec / chat / persona text. Title 1–300 and
/// observe-note 4000 are other cards. A 32001-char policy must not be stored
/// or stuffed into the wake prompt.
#[tokio::test]
async fn cases_policy_md_over_32k_is_400_and_inserts_no_row() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    let snap = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "pre-call GET /cases: {}",
        snap.body
    );
    let snap_rows = listed_case_id_titles(&snap.body);
    let snap_count = snap.body["count"]
        .as_u64()
        .unwrap_or_else(|| snap_rows.len() as u64);
    assert_eq!(
        snap_count as usize,
        snap_rows.len(),
        "snapshot count must match cases array: {}",
        snap.body
    );
    assert!(
        !snap.raw.contains(&huge),
        "pre-call GET /cases must not already contain the 32001-char string"
    );

    let over_title = format!("policy-over-32k-{}", Uuid::new_v4());
    let over = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": over_title,
            "policy_md": huge,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_ne!(
        over.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "32001-char policy_md must not 500: {}",
        over.body
    );
    assert_ne!(
        over.status,
        StatusCode::OK,
        "32001-char policy_md must not 200 (stores the 32k+ string): {}",
        over.body
    );
    assert_ne!(
        over.status,
        StatusCode::CREATED,
        "32001-char policy_md must not 201 (stores the 32k+ string): {}",
        over.body
    );
    assert_eq!(
        over.status,
        StatusCode::BAD_REQUEST,
        "32001-char policy_md must be 400: {}",
        over.body
    );
    let err = http_error_text(&over).to_ascii_lowercase();
    assert!(
        err.contains("too long")
            || err.contains("32k")
            || err.contains("32000")
            || err.contains("32_000"),
        "400 must be policy_md too long (or the 32k family), got {:?}: {}",
        over.body.get("error"),
        over.body
    );
    assert!(
        !over.raw.contains(&huge),
        "400 body must not echo the 32001-char policy_md: {}",
        over.status
    );
    assert!(
        over.body.get("id").and_then(|v| v.as_str()).is_none(),
        "400 must not return a case id: {}",
        over.body
    );

    let after = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /cases after reject: {}",
        after.body
    );
    let after_rows = listed_case_id_titles(&after.body);
    let after_count = after.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_rows.len() as u64);
    assert_eq!(
        after_count, snap_count,
        "rejected 32001-char policy_md must not change GET /cases count: snap={snap_count} after={after_count} body={}",
        after.body
    );
    assert_eq!(
        after_rows, snap_rows,
        "GET /cases after reject must match the snapshot (no extra row): snap={snap_rows:?} after={after_rows:?} body={}",
        after.body
    );
    assert!(
        after_rows.iter().all(|(_, title)| title != &over_title),
        "rejected title must not appear on GET /cases: {}",
        after.body
    );
    assert!(
        !after.raw.contains(&huge)
            && !after.body.to_string().contains(&huge)
            && after.body["cases"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|c| c["policy_md"].as_str() != Some(huge.as_str())),
        "GET /cases after reject must not contain the 32001-char string"
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };
    let tool_title = format!("policy-over-tool-{}", Uuid::new_v4());
    let (tool_content, tool_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "start_case",
        &json!({
            "title": tool_title,
            "policy_md": huge,
            "wake_in_s": 3600
        }),
        &llm,
    )
    .await;
    assert!(
        !tool_action.ok,
        "start_case 32001-char policy_md must fail closed: action={tool_action:?} content={tool_content}"
    );
    assert_eq!(
        tool_action.run_id, None,
        "start_case must not return a run_id: {tool_action:?}"
    );
    assert!(
        tool_content.get("id").and_then(|v| v.as_str()).is_none(),
        "start_case reject must not return a case id: {tool_content}"
    );
    let tool_err = tool_content
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or(tool_action.summary.as_str())
        .to_ascii_lowercase();
    assert!(
        !tool_err.contains("internal"),
        "start_case 32001-char policy_md must fail closed (400), not 500: action={tool_action:?} content={tool_content}"
    );
    assert!(
        tool_err.contains("too long")
            || tool_err.contains("32k")
            || tool_err.contains("32000")
            || tool_err.contains("32_000"),
        "start_case 400 must be policy_md too long (or the 32k family): action={tool_action:?} content={tool_content}"
    );

    let after_tool = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        after_tool.status,
        StatusCode::OK,
        "GET /cases after start_case reject: {}",
        after_tool.body
    );
    let after_tool_rows = listed_case_id_titles(&after_tool.body);
    let after_tool_count = after_tool.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_tool_rows.len() as u64);
    assert_eq!(
        after_tool_count, snap_count,
        "start_case 32001-char policy_md must not insert: snap={snap_count} after={after_tool_count} body={}",
        after_tool.body
    );
    assert!(
        after_tool_rows
            .iter()
            .all(|(_, title)| title != &tool_title),
        "start_case rejected title must not appear on GET /cases: {}",
        after_tool.body
    );
    assert!(
        !after_tool.raw.contains(&huge)
            && after_tool.body["cases"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|c| c["policy_md"].as_str() != Some(huge.as_str())),
        "GET /cases after start_case reject must not contain the 32001-char string"
    );

    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(ws, b_ws, "A and B must have distinct workspaces");
    let stolen_title = format!("policy-stolen-{}", Uuid::new_v4());
    let stolen_policy = format!("unique-policy-{}", Uuid::new_v4());
    let create_as_b = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&b_jar),
        Some(json!({
            "title": stolen_title,
            "policy_md": stolen_policy,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        create_as_b.status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A's POST /cases must be 403: {}",
        create_as_b.body
    );
    let after_b = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(after_b.status, StatusCode::OK, "{}", after_b.body);
    assert!(
        listed_case_id_titles(&after_b.body)
            .iter()
            .all(|(_, title)| title != &stolen_title)
            && !after_b.raw.contains(&stolen_policy),
        "other-ws POST must not insert on A: {}",
        after_b.body
    );

    let short_title = format!("policy-short-{}", Uuid::new_v4());
    let short_policy = format!("short-unique-policy-{}", Uuid::new_v4());
    let short = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": short_title,
            "policy_md": short_policy,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        short.status,
        StatusCode::OK,
        "control short unique policy_md must 200 (not a tautology that every create 400s): {}",
        short.body
    );
    assert_eq!(
        short.body["policy_md"].as_str(),
        Some(short_policy.as_str()),
        "control POST must persist the short unique policy_md: {}",
        short.body
    );
    let short_id = short.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control case id: {}", short.body))
        .to_string();
    let short_got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{short_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(short_got.status, StatusCode::OK, "{}", short_got.body);
    assert_eq!(
        short_got.body["policy_md"].as_str(),
        Some(short_policy.as_str()),
        "GET /cases/{{id}} must show the short unique policy: {}",
        short_got.body
    );

    let omitted_title = format!("policy-omitted-{}", Uuid::new_v4());
    let omitted = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": omitted_title,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        omitted.status,
        StatusCode::OK,
        "control omitted policy_md must 200: {}",
        omitted.body
    );
    let omitted_id = omitted.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("omitted case id: {}", omitted.body))
        .to_string();
    let omitted_got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{omitted_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(omitted_got.status, StatusCode::OK, "{}", omitted_got.body);
    let omitted_policy = omitted_got.body["policy_md"].as_str().unwrap_or("\0");
    assert_ne!(
        omitted_policy,
        huge.as_str(),
        "omitted policy_md must not store the 32001-char string: {}",
        omitted_got.body
    );
    assert!(
        omitted_policy.len() <= 32_000,
        "omitted policy_md must not store a 32k+ string: {}",
        omitted_got.body
    );

    let at_cap = "y".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);
    let cap_title = format!("policy-at-cap-{}", Uuid::new_v4());
    let cap_out = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": cap_title,
            "policy_md": at_cap,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_ne!(
        cap_out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "32_000-char policy_md must not 500: {}",
        cap_out.body
    );
    if cap_out.status == StatusCode::OK {
        let cap_id = cap_out.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("at-cap case id: {}", cap_out.body))
            .to_string();
        assert_eq!(
            cap_out.body["policy_md"].as_str(),
            Some(at_cap.as_str()),
            "200 at the 32k boundary must persist the 32_000-char policy: {}",
            cap_out.body
        );
        let cap_got = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{cap_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(cap_got.status, StatusCode::OK, "{}", cap_got.body);
        assert_eq!(
            cap_got.body["policy_md"].as_str(),
            Some(at_cap.as_str()),
            "GET /cases/{{id}} at the 32k boundary must show the 32_000-char policy: {}",
            cap_got.body
        );
    } else {
        assert_eq!(
            cap_out.status,
            StatusCode::BAD_REQUEST,
            "32_000-char policy_md may 200, or 400 if the documented cap is lower: {}",
            cap_out.body
        );
        let after_cap = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
        assert_eq!(after_cap.status, StatusCode::OK, "{}", after_cap.body);
        assert!(
            listed_case_id_titles(&after_cap.body)
                .iter()
                .all(|(_, title)| title != &cap_title),
            "lower-cap reject must not insert the 32_000-char row: {}",
            after_cap.body
        );
    }
}

/// Observe on acting must keep `status=acting` and the fleet `run_id`.
/// HTTP default `wake=true` must not reopen the case, bump `next_wake_at`,
/// or insert a second accepted plan. Waiting + wake=true still reopens —
/// acting is the exception.
#[tokio::test]
async fn observe_on_acting_keeps_status_acting_and_does_not_clobber_run_id() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let one_task = json!({
        "decision": {
            "action": "launch_agents",
            "launch": {
                "title": "one-task",
                "tasks": [{"id": "t1", "title": "T1", "spec": "do the work"}]
            }
        }
    });

    let title = format!("observe-on-acting-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &jar, &ws, &title).await;
    let launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(one_task),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
    assert_eq!(
        launched.body["status"], "acting",
        "one-task launch_agents must set acting: {}",
        launched.body
    );
    assert_ne!(launched.body["status"], "open", "{}", launched.body);
    assert_ne!(launched.body["status"], "waiting", "{}", launched.body);
    assert_ne!(launched.body["status"], "deciding", "{}", launched.body);
    assert_ne!(launched.body["status"], "cancelled", "{}", launched.body);
    let run_id = launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("launch must return run_id uuid: {}", launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&run_id).is_ok(),
        "run_id must be a uuid: {run_id:?} body={}",
        launched.body
    );
    assert!(
        launched.body["next_wake_at"].is_null(),
        "launch nulls next_wake_at: {}",
        launched.body
    );
    let decision_count = launched.body["decision_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("decision_count: {}", launched.body));

    let got_acting = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_acting.status, StatusCode::OK, "{}", got_acting.body);
    assert_eq!(
        got_acting.body["status"], "acting",
        "GET after launch_agents: {}",
        got_acting.body
    );
    assert_eq!(
        got_acting.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET must persist the same run_id: {}",
        got_acting.body
    );
    assert!(
        got_acting.body["next_wake_at"].is_null(),
        "GET after launch next_wake_at: {}",
        got_acting.body
    );

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);
    assert!(
        snap_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must list the launched run {run_id}: {}",
        snap.body
    );
    assert_eq!(
        snap_ids,
        vec![run_id.clone()],
        "this workspace must have only that first run before observe: {snap_ids:?} body={}",
        snap.body
    );

    // HTTP default is wake=true (`unwrap_or(true)`). Must still keep acting.
    let observed = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "human fact while agents run" })),
    )
    .await;
    assert_eq!(observed.status, StatusCode::OK, "{}", observed.body);
    assert_eq!(
        observed.body["status"], "acting",
        "observe (default wake=true) on acting must keep acting: {}",
        observed.body
    );
    assert_ne!(
        observed.body["status"], "open",
        "observe must not reset acting → open (drops the fleet run link): {}",
        observed.body
    );
    assert_ne!(observed.body["status"], "waiting", "{}", observed.body);
    assert_ne!(observed.body["status"], "deciding", "{}", observed.body);
    assert_ne!(observed.body["status"], "cancelled", "{}", observed.body);
    assert_eq!(
        observed.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "observe must not clobber run_id: {}",
        observed.body
    );
    assert!(
        observed.body["next_wake_at"].is_null(),
        "observe must not wake acting; next_wake_at stays null: {}",
        observed.body
    );
    assert_eq!(
        observed.body["decision_count"].as_i64(),
        Some(decision_count),
        "observe must not bump decision_count: {}",
        observed.body
    );
    assert_eq!(
        observed.body["context"]["fleet_run"]["id"].as_str(),
        Some(run_id.as_str()),
        "observe must keep context.fleet_run: {}",
        observed.body
    );
    let notes = observed.body["context"]["observations"]
        .as_array()
        .unwrap_or_else(|| panic!("observations: {}", observed.body));
    assert!(
        notes
            .iter()
            .any(|n| n["note"].as_str() == Some("human fact while agents run")),
        "observe must record the note: {}",
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
    assert_eq!(
        got.body["status"], "acting",
        "GET after observe must stay acting: {}",
        got.body
    );
    assert_ne!(got.body["status"], "open", "{}", got.body);
    assert_ne!(got.body["status"], "waiting", "{}", got.body);
    assert_ne!(got.body["status"], "deciding", "{}", got.body);
    assert_ne!(got.body["status"], "cancelled", "{}", got.body);
    assert_eq!(
        got.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET after observe must keep the same run_id: {}",
        got.body
    );
    assert!(
        got.body["next_wake_at"].is_null(),
        "GET next_wake_at after observe: {}",
        got.body
    );
    assert_eq!(
        got.body["decision_count"].as_i64(),
        Some(decision_count),
        "GET decision_count after observe: {}",
        got.body
    );
    assert_eq!(
        got.body["context"]["fleet_run"]["id"].as_str(),
        Some(run_id.as_str()),
        "GET context.fleet_run: {}",
        got.body
    );
    let got_notes = got.body["context"]["observations"]
        .as_array()
        .unwrap_or_else(|| panic!("GET observations: {}", got.body));
    assert!(
        got_notes
            .iter()
            .any(|n| n["note"].as_str() == Some("human fact while agents run")),
        "GET must include the note: {}",
        got.body
    );
    let events = got.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got.body));
    assert!(
        events.iter().any(|e| {
            e["kind"].as_str() == Some("observed")
                && e["payload"]["note"].as_str() == Some("human fact while agents run")
        }),
        "events must include observed with matching note: {}",
        got.body
    );

    let after_obs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(after_obs.status, StatusCode::OK, "{}", after_obs.body);
    let after_obs_ids = fleet_run_ids(&after_obs.body);
    assert!(
        after_obs_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must still list first id {run_id}: {}",
        after_obs.body
    );
    assert_eq!(
        after_obs_ids, snap_ids,
        "observe must not grow a second accepted plan: snap={snap_ids:?} after={after_obs_ids:?} body={}",
        after_obs.body
    );

    let again = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "second fact", "wake": false })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(
        again.body["status"], "acting",
        "observe wake=false on acting must keep acting: {}",
        again.body
    );
    assert_ne!(again.body["status"], "open", "{}", again.body);
    assert_ne!(again.body["status"], "waiting", "{}", again.body);
    assert_ne!(again.body["status"], "deciding", "{}", again.body);
    assert_ne!(again.body["status"], "cancelled", "{}", again.body);
    assert_eq!(
        again.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "observe wake=false must keep run_id: {}",
        again.body
    );
    assert!(
        again.body["next_wake_at"].is_null(),
        "observe wake=false must not set next_wake_at: {}",
        again.body
    );
    assert_eq!(
        again.body["decision_count"].as_i64(),
        Some(decision_count),
        "second observe must not bump decision_count: {}",
        again.body
    );

    let wait_title = format!("observe-on-acting-wait-{}", Uuid::new_v4());
    let wait_id = create_open_case(&app, &jar, &ws, &wait_title).await;
    let waited = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{wait_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "wait", "wait_seconds": 3600 }
        })),
    )
    .await;
    assert_eq!(waited.status, StatusCode::OK, "{}", waited.body);
    assert_eq!(
        waited.body["status"], "waiting",
        "control wait must set waiting: {}",
        waited.body
    );
    assert!(
        waited.body["run_id"].is_null(),
        "wait must not invent a run_id: {}",
        waited.body
    );
    let far_wake = parse_next_wake_at(&waited.body);

    let got_wait = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{wait_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_wait.status, StatusCode::OK, "{}", got_wait.body);
    assert_eq!(got_wait.body["status"], "waiting", "{}", got_wait.body);
    assert!(got_wait.body["run_id"].is_null(), "{}", got_wait.body);
    let far_wake_get = parse_next_wake_at(&got_wait.body);
    assert_eq!(
        far_wake_get, far_wake,
        "GET waiting next_wake_at must match decide: {}",
        got_wait.body
    );

    let nudged = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{wait_id}/observe")),
        Some(&jar),
        Some(json!({ "note": "nudge waiting", "wake": true })),
    )
    .await;
    assert_eq!(nudged.status, StatusCode::OK, "{}", nudged.body);
    assert_eq!(
        nudged.body["status"], "open",
        "observe wake=true on waiting still reopens; acting is the exception: {}",
        nudged.body
    );
    assert_ne!(
        nudged.body["status"], "acting",
        "waiting observe must not become acting: {}",
        nudged.body
    );
    let nudged_wake = parse_next_wake_at(&nudged.body);
    assert!(
        nudged_wake < far_wake,
        "wake=true on waiting must schedule sooner than the far-future wait: nudged={nudged_wake} far={far_wake} body={}",
        nudged.body
    );

    let after_nudge = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(after_nudge.status, StatusCode::OK, "{}", after_nudge.body);
    let after_nudge_ids = fleet_run_ids(&after_nudge.body);
    assert_eq!(
        after_nudge_ids, snap_ids,
        "waiting observe must not grow fleet runs: snap={snap_ids:?} after={after_nudge_ids:?} body={}",
        after_nudge.body
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

    let plane_id = fleet_db::control_plane_runner_id(workspace_id);
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
    let created_here: Vec<_> = host_list
        .iter()
        .filter(|h| {
            h["workspace_id"].as_str() == Some(ws.as_str()) && h["kind"] != "control_plane"
        })
        .collect();
    assert!(
        created_here.is_empty(),
        "ack must not create worker_hosts in this workspace: {}",
        hosts.body
    );
    assert!(
        host_list.iter().any(|h| {
            h["id"].as_str() == Some(plane_id.to_string().as_str())
                && h["kind"] == "control_plane"
        }),
        "plane row must still be listed: {}",
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

fn assert_partners_disclaimer_other_ws_forbidden(out: &HttpOut, label: &str, leaks: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not be 200 accepted=true: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    assert_ne!(
        out.body.get("accepted"),
        Some(&json!(true)),
        "{label} must not return accepted=true: {}",
        out.body
    );
    if let Some(at) = out.body.get("accepted_at") {
        assert!(
            at.is_null(),
            "{label} must not include a timestamp for A: {}",
            out.body
        );
    }
    for secret in leaks {
        assert!(
            !out.raw.contains(secret),
            "{label} body must not contain A's accepted_at {secret:?}: {}",
            out.raw
        );
        assert!(
            !out.body.to_string().contains(secret),
            "{label} json must not contain A's accepted_at {secret:?}: {}",
            out.body
        );
    }
}

/// B's cookie on A's GET/POST /partners/disclaimer is 403 not-a-member and
/// must not read or stamp A's `accepted_at`. Distinct from
/// `partners_disclaimer_get_defaults_not_accepted` /
/// `partners_disclaimer_post_then_get_ack_is_sticky` /
/// `partners_disclaimer_ack_does_not_enqueue_or_create_worker_hosts`
/// (same-workspace only). Distinct from list-partners-page-coming-soon-copy
/// (Coming soon UI) and list-partners-ack-not-assignable (ack ≠ assignable).
/// Filter `-- partners_disclaimer_other_ws`.
#[tokio::test]
async fn partners_disclaimer_other_ws_is_403_and_does_not_ack() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must own distinct workspaces");

    let snap = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /partners/disclaimer: {}",
        snap.body
    );
    assert_eq!(
        snap.body["accepted"], false,
        "default must be not accepted: {}",
        snap.body
    );
    assert!(
        snap.body["accepted_at"].is_null(),
        "default accepted_at must be null: {}",
        snap.body
    );

    let get_as_b = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&b_jar), None).await;
    assert_partners_disclaimer_other_ws_forbidden(&get_as_b, "B GET A's /partners/disclaimer", &[]);

    let post_as_b = call(
        &app,
        "POST",
        &partners_disclaimer(&a_ws),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_partners_disclaimer_other_ws_forbidden(
        &post_as_b,
        "B POST A's /partners/disclaimer",
        &[],
    );

    let after_b = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        after_b.status,
        StatusCode::OK,
        "A GET /partners/disclaimer after B: {}",
        after_b.body
    );
    assert_eq!(
        after_b.body, snap.body,
        "B GET/POST must not stamp A's accepted_at: before={} after={}",
        snap.body, after_b.body
    );
    assert_eq!(
        after_b.body["accepted"], false,
        "A must still be accepted=false after B: {}",
        after_b.body
    );
    assert!(
        after_b.body["accepted_at"].is_null(),
        "B POST must not write a new timestamp for A: {}",
        after_b.body
    );

    let a_posted = call(
        &app,
        "POST",
        &partners_disclaimer(&a_ws),
        Some(&a_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        a_posted.status,
        StatusCode::OK,
        "A POST /partners/disclaimer: {}",
        a_posted.body
    );
    assert_eq!(
        a_posted.body["accepted"], true,
        "A POST must record accepted=true: {}",
        a_posted.body
    );
    let a_accepted_at = a_posted.body["accepted_at"]
        .as_str()
        .unwrap_or_else(|| panic!("A POST accepted_at: {}", a_posted.body))
        .to_string();
    assert!(
        !a_accepted_at.is_empty(),
        "A POST must return accepted_at: {}",
        a_posted.body
    );

    let get_as_b_after = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&b_jar), None).await;
    assert_partners_disclaimer_other_ws_forbidden(
        &get_as_b_after,
        "B GET A's /partners/disclaimer after A ack",
        &[a_accepted_at.as_str()],
    );

    let post_as_b_after = call(
        &app,
        "POST",
        &partners_disclaimer(&a_ws),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_partners_disclaimer_other_ws_forbidden(
        &post_as_b_after,
        "B POST A's /partners/disclaimer after A ack",
        &[a_accepted_at.as_str()],
    );

    let a_again = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        a_again.status,
        StatusCode::OK,
        "A GET after second B POST: {}",
        a_again.body
    );
    assert_eq!(
        a_again.body["accepted"], true,
        "A must stay accepted after B POST: {}",
        a_again.body
    );
    assert_eq!(
        a_again.body["accepted_at"].as_str(),
        Some(a_accepted_at.as_str()),
        "second B POST must not overwrite first-write-wins: {}",
        a_again.body
    );

    let b_own = call(&app, "GET", &partners_disclaimer(&b_ws), Some(&b_jar), None).await;
    assert_eq!(
        b_own.status,
        StatusCode::OK,
        "B cookie GET B /partners/disclaimer must be 200 so this is not a tautology that every disclaimer call is 403: {}",
        b_own.body
    );
    assert_eq!(
        b_own.body["accepted"], false,
        "A's ack must not appear on B: {}",
        b_own.body
    );
    assert!(
        b_own.body["accepted_at"].is_null(),
        "A's accepted_at must not appear on B: {}",
        b_own.body
    );
    assert!(
        !b_own.raw.contains(&a_accepted_at),
        "B GET B must not include A's accepted_at: {}",
        b_own.raw
    );

    let b_posted = call(
        &app,
        "POST",
        &partners_disclaimer(&b_ws),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        b_posted.status,
        StatusCode::OK,
        "control: B POST B /partners/disclaimer must be 200 so this is not a tautology that every disclaimer call is 403: {}",
        b_posted.body
    );
    assert_eq!(
        b_posted.body["accepted"], true,
        "control: B POST B must record B's own ack: {}",
        b_posted.body
    );
    let b_accepted_at = b_posted.body["accepted_at"]
        .as_str()
        .unwrap_or_else(|| panic!("B POST accepted_at: {}", b_posted.body))
        .to_string();
    assert!(
        !b_accepted_at.is_empty(),
        "control: B POST must return accepted_at: {}",
        b_posted.body
    );
    assert_ne!(
        b_accepted_at, a_accepted_at,
        "A's ack must not appear on B: A={a_accepted_at} B={b_accepted_at}"
    );

    let a_final = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        a_final.status,
        StatusCode::OK,
        "A GET after B's own ack: {}",
        a_final.body
    );
    assert_eq!(
        a_final.body["accepted_at"].as_str(),
        Some(a_accepted_at.as_str()),
        "B's own ack must not change A's accepted_at: {}",
        a_final.body
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

fn parse_next_wake_at(body: &Value) -> chrono::DateTime<chrono::Utc> {
    let raw = body["next_wake_at"]
        .as_str()
        .unwrap_or_else(|| panic!("next_wake_at must be an RFC3339 string: {body}"));
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("next_wake_at rfc3339 {raw}: {e}"))
        .with_timezone(&chrono::Utc)
}

/// Create-path lock: 200 must carry a parseable `next_wake_at` inside
/// `[earliest, latest]` (`now + MAX_WAIT_SECS + 2s`). Far-future years
/// (`+33715-…`) fail RFC3339 — that is the unclamped 10^12 overflow, not a
/// parser quirk to paper over.
fn parse_create_wake_within_max(
    body: &Value,
    earliest: chrono::DateTime<chrono::Utc>,
    latest: chrono::DateTime<chrono::Utc>,
    label: &str,
) -> chrono::DateTime<chrono::Utc> {
    let raw = body["next_wake_at"]
        .as_str()
        .unwrap_or_else(|| panic!("{label}: next_wake_at must be an RFC3339 string: {body}"));
    let next = chrono::DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|e| {
            panic!(
                "{label}: next_wake_at must be within now+MAX_WAIT_SECS \
                 (got {raw:?}: {e}; 10^12s is year ~33715 if unclamped): {body}"
            )
        });
    assert!(
        next >= earliest,
        "{label}: next_wake_at must not be in the past (ticker spin): next={next} min={earliest} body={body}"
    );
    assert!(
        next <= latest,
        "{label}: next_wake_at must be within now+MAX_WAIT_SECS (no overflow / far-future abuse): next={next} max={latest} body={body}"
    );
    next
}

fn fleet_run_ids(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("runs array: {body}"))
        .iter()
        .map(|r| {
            r["id"]
                .as_str()
                .unwrap_or_else(|| panic!("run id: {r}"))
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

fn fleet_task_ids(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks array: {body}"))
        .iter()
        .map(|t| {
            t["id"]
                .as_str()
                .unwrap_or_else(|| panic!("task id: {t}"))
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

fn fleet_task_titles(body: &Value) -> Vec<String> {
    body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks array: {body}"))
        .iter()
        .map(|t| {
            t["title"]
                .as_str()
                .unwrap_or_else(|| panic!("task title: {t}"))
                .to_string()
        })
        .collect()
}

fn assert_case_denied(out: &HttpOut, case_id: &str, title: &str) {
    assert!(
        out.status == StatusCode::NOT_FOUND || out.status == StatusCode::FORBIDDEN,
        "must be 403/404, never leak a case: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "must not return 200 with another workspace's case: {}",
        out.body
    );
    if out.body.get("id").and_then(|v| v.as_str()) == Some(case_id) {
        panic!("leaked case id {case_id} in {}: {}", out.status, out.body);
    }
    if out.body.get("title").and_then(|v| v.as_str()) == Some(title) {
        panic!("leaked case title {title} in {}: {}", out.status, out.body);
    }
}

fn assert_inbox_denied(out: &HttpOut) {
    assert!(
        out.status == StatusCode::FORBIDDEN || out.status == StatusCode::UNAUTHORIZED,
        "other workspace inbox must be 403/401, not 200 with data: {} {}",
        out.status,
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "must not return inbox data: {}",
        out.body
    );
}

#[tokio::test]
async fn cases_other_workspace_cannot_get_observe_decide_cancel_wake() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;

    let title = "tenant-a-secret-case";
    let case_id = create_open_case(&app, &a_jar, &a_ws, title).await;

    let observe_body = json!({ "note": "should not land", "wake": false });
    let decide_body = json!({
        "decision": { "action": "wait", "reason": "cross-tenant" }
    });

    // B hitting A's workspace routes.
    let a_routes = [
        ("GET", cases_path(&a_ws, &format!("/{case_id}")), None),
        (
            "POST",
            cases_path(&a_ws, &format!("/{case_id}/observe")),
            Some(observe_body.clone()),
        ),
        (
            "POST",
            cases_path(&a_ws, &format!("/{case_id}/decide")),
            Some(decide_body.clone()),
        ),
        (
            "POST",
            cases_path(&a_ws, &format!("/{case_id}/cancel")),
            Some(json!({})),
        ),
        (
            "POST",
            cases_path(&a_ws, &format!("/{case_id}/wake")),
            Some(json!({})),
        ),
    ];
    for (method, uri, body) in &a_routes {
        let out = call(&app, method, uri, Some(&b_jar), body.clone()).await;
        assert_case_denied(&out, &case_id, title);
    }

    // IDOR: B's workspace + A's case id must not resolve A's row.
    let b_routes = [
        ("GET", cases_path(&b_ws, &format!("/{case_id}")), None),
        (
            "POST",
            cases_path(&b_ws, &format!("/{case_id}/observe")),
            Some(observe_body),
        ),
        (
            "POST",
            cases_path(&b_ws, &format!("/{case_id}/decide")),
            Some(decide_body),
        ),
        (
            "POST",
            cases_path(&b_ws, &format!("/{case_id}/cancel")),
            Some(json!({})),
        ),
        (
            "POST",
            cases_path(&b_ws, &format!("/{case_id}/wake")),
            Some(json!({})),
        ),
    ];
    for (method, uri, body) in &b_routes {
        let out = call(&app, method, uri, Some(&b_jar), body.clone()).await;
        assert_case_denied(&out, &case_id, title);
    }

    let still = call(
        &app,
        "GET",
        &cases_path(&a_ws, &format!("/{case_id}")),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["id"], case_id);
    assert_eq!(still.body["title"], title);
    assert_eq!(still.body["status"], "open");
}

/// `get_case` / `observe_case` / `cancel_case` reject another workspace's
/// `case_id` even when the same user is a member of both. Distinct from
/// `cases_other_workspace_cannot_get_observe_decide_cancel_wake` (cookie
/// HTTP) and payload `workspace_id` (`require_session_workspace`). Filter
/// `-- case_id_workspace`.
#[tokio::test]
async fn chat_tools_get_observe_cancel_case_id_workspace_scope() {
    let (app, state) = app_with_state().await;
    let (_email, jar, a_ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let a_ws_id = Uuid::parse_str(&a_ws).expect("a_ws");

    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("case-id-ws-b-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "POST /api/workspaces B: {}",
        b_created.body
    );
    let b_ws = b_created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.id: {}", b_created.body))
        .to_string();
    let b_ws_id = Uuid::parse_str(&b_ws).expect("b_ws");
    assert_ne!(a_ws_id, b_ws_id, "A and B must be distinct workspaces");

    let secret_title = format!("A-secret-case-title-{}", Uuid::new_v4());
    let secret_policy = format!("A-secret-policy-md-{}", Uuid::new_v4());
    let secret_observe = format!("A-secret-observation-{}", Uuid::new_v4());
    let b_observe = format!("B-must-not-land-{}", Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &cases_path(&a_ws, ""),
        Some(&jar),
        Some(json!({
            "title": secret_title,
            "policy_md": secret_policy,
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create case on A: {}",
        created.body
    );
    let a_case_id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case id: {}", created.body))
        .to_string();

    let planted = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{a_case_id}/observe")),
        Some(&jar),
        Some(json!({ "note": secret_observe, "wake": false })),
    )
    .await;
    assert_eq!(
        planted.status,
        StatusCode::OK,
        "plant observation on A: {}",
        planted.body
    );

    let snap = call(
        &app,
        "GET",
        &cases_path(&a_ws, &format!("/{a_case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "A GET /cases/{{id}} snapshot: {}",
        snap.body
    );
    assert_eq!(snap.body["id"], a_case_id, "{}", snap.body);
    assert_eq!(snap.body["title"], secret_title, "{}", snap.body);
    assert_eq!(snap.body["policy_md"], secret_policy, "{}", snap.body);
    assert_eq!(snap.body["status"], "open", "{}", snap.body);
    let snap_version = snap.body["version"].clone();
    let snap_kinds = case_event_kinds(&snap.body);
    let snap_notes = case_observation_notes(&snap.body);
    assert!(
        snap_kinds.iter().any(|k| k == "observed"),
        "snapshot must include the planted observe event so a missed extra observe is visible: {snap_kinds:?} {}",
        snap.body
    );
    assert!(
        snap_notes.iter().any(|n| n == &secret_observe),
        "snapshot must include the planted observation so a leak check is not vacuous: {snap_notes:?} {}",
        snap.body
    );

    let llm = fleet_server::chat_tools::ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.5".into(),
        runner_id: None,
        effort: None,
    };

    let secrets = [
        secret_title.as_str(),
        secret_policy.as_str(),
        secret_observe.as_str(),
    ];

    for (tool, args) in [
        ("get_case", json!({ "case_id": a_case_id })),
        (
            "observe_case",
            json!({
                "case_id": a_case_id,
                "note": b_observe,
                "wake": false
            }),
        ),
        ("cancel_case", json!({ "case_id": a_case_id })),
    ] {
        let (content, action) =
            fleet_server::chat_tools::execute_tool(&state, b_ws_id, user_id, tool, &args, &llm)
                .await;
        assert_case_tool_foreign_case_rejected(tool, &action, &content, &secrets);
    }

    let still = call(
        &app,
        "GET",
        &cases_path(&a_ws, &format!("/{a_case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "A GET after B tools: {}",
        still.body
    );
    assert_eq!(still.body["id"], a_case_id, "{}", still.body);
    assert_eq!(still.body["title"], secret_title, "{}", still.body);
    assert_eq!(still.body["policy_md"], secret_policy, "{}", still.body);
    assert_eq!(
        still.body["status"], "open",
        "B must not cancel A's case: {}",
        still.body
    );
    assert_eq!(
        still.body["version"], snap_version,
        "B must not patch A's case: snap={snap_version} after={} body={}",
        still.body["version"], still.body
    );
    assert_eq!(
        case_event_kinds(&still.body),
        snap_kinds,
        "B must not append an observe/cancel event on A: {}",
        still.body
    );
    assert_eq!(
        case_observation_notes(&still.body),
        snap_notes,
        "B must not add an observation on A: {}",
        still.body
    );
    let still_s = still.body.to_string();
    assert!(
        !still_s.contains(&b_observe),
        "A's case must not include B's observe note: {}",
        still.body
    );

    let (a_got, a_get) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "get_case",
        &json!({ "case_id": a_case_id }),
        &llm,
    )
    .await;
    assert!(
        a_get.ok,
        "control: get_case on A's workspace with A's case_id must work so this is not a tautology that every get_case 404s: {a_get:?} {a_got}"
    );
    assert_eq!(a_got["id"].as_str(), Some(a_case_id.as_str()), "{a_got}");
    assert_eq!(
        a_got["title"].as_str(),
        Some(secret_title.as_str()),
        "{a_got}"
    );
    assert_eq!(
        a_got["policy_md"].as_str(),
        Some(secret_policy.as_str()),
        "{a_got}"
    );
    assert_eq!(a_got["status"], "open", "{a_got}");
    let a_got_notes = case_observation_notes(&a_got);
    assert!(
        a_got_notes.iter().any(|n| n == &secret_observe),
        "A-side get_case must return the planted observation: {a_got_notes:?} {a_got}"
    );

    let (a_cancel_body, a_cancel) = fleet_server::chat_tools::execute_tool(
        &state,
        a_ws_id,
        user_id,
        "cancel_case",
        &json!({ "case_id": a_case_id }),
        &llm,
    )
    .await;
    let mut a_ids = vec![a_case_id.clone()];
    let mut a_titles = vec![secret_title.clone()];
    if a_cancel.ok {
        assert_eq!(
            a_cancel_body["status"], "cancelled",
            "A-side cancel_case 200 must cancel A: {a_cancel:?} {a_cancel_body}"
        );
        let second_title = format!("A-second-open-{}", Uuid::new_v4());
        let second_id = create_open_case(&app, &jar, &a_ws, &second_title).await;
        a_ids.push(second_id);
        a_titles.push(second_title);
    }

    let (listed, list_action) = fleet_server::chat_tools::execute_tool(
        &state,
        b_ws_id,
        user_id,
        "list_cases",
        &json!({}),
        &llm,
    )
    .await;
    assert!(
        list_action.ok,
        "control: list_cases on B must succeed so this is not a tautology that every case tool 404s: {list_action:?} {listed}"
    );
    let listed_s = format!("{list_action:?}{listed}");
    for (id, title) in a_ids.iter().zip(a_titles.iter()) {
        assert!(
            !listed_s.contains(id.as_str()) && !listed_s.contains(title.as_str()),
            "list_cases on B must not include A's case {id} / {title}: {listed}"
        );
    }
    if let Some(rows) = listed.get("cases").and_then(|v| v.as_array()) {
        for row in rows {
            let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let title = row.get("title").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                !a_ids.iter().any(|x| x == id) && !a_titles.iter().any(|x| x == title),
                "list_cases on B must not list A's case: {listed}"
            );
        }
    }
}

fn case_event_kinds(body: &Value) -> Vec<String> {
    body.get("events")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("events array: {body}"))
        .iter()
        .map(|e| {
            e.get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("event kind: {e}"))
                .to_string()
        })
        .collect()
}

fn case_observation_notes(body: &Value) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(arr) = body
        .pointer("/context/observations")
        .and_then(|v| v.as_array())
    {
        for o in arr {
            if let Some(n) = o.get("note").and_then(|v| v.as_str()) {
                notes.push(n.to_string());
            }
        }
    }
    if let Some(events) = body.get("events").and_then(|v| v.as_array()) {
        for e in events {
            if e.get("kind").and_then(|v| v.as_str()) == Some("observed") {
                if let Some(n) = e.pointer("/payload/note").and_then(|v| v.as_str()) {
                    notes.push(n.to_string());
                }
            }
        }
    }
    notes
}

fn assert_case_tool_foreign_case_rejected(
    tool: &str,
    action: &fleet_server::chat_tools::ChatAction,
    body: &Value,
    secrets: &[&str],
) {
    assert!(
        !action.ok,
        "{tool} with another workspace's case_id must be ok:false: {action:?} {body}"
    );
    assert_eq!(
        action.run_id, None,
        "{tool} must not echo a foreign case as a run: {action:?}"
    );
    let err = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or(action.summary.as_str());
    let lower = err.to_ascii_lowercase();
    assert!(
        !lower.contains("internal server")
            && !lower.contains("database error")
            && !lower.contains("database unavailable")
            && !err.contains("500"),
        "{tool} must not 500: {err} {body}"
    );
    assert!(
        lower.contains("case not found"),
        "{tool} must fail closed 404 case not found, not succeed as A: {err} {body}"
    );
    if body.get("id").and_then(|v| v.as_str()).is_some() && body.get("title").is_some() {
        panic!("{tool} must not return a case body: {body}");
    }
    let dumped = format!("{action:?}{body}");
    for secret in secrets {
        assert!(
            !dumped.contains(secret),
            "{tool} body / action must not include {secret:?}: {dumped}"
        );
    }
}

fn listed_case_id_titles(body: &Value) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases array: {body}"))
        .iter()
        .map(|c| {
            let id = c["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case id: {c}"))
                .to_string();
            let title = c["title"]
                .as_str()
                .unwrap_or_else(|| panic!("case title: {c}"))
                .to_string();
            (id, title)
        })
        .collect();
    rows.sort();
    rows
}

fn assert_cases_not_a_member(out: &HttpOut, label: &str, leaks: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{label} must not be 200: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{label} must not be 201: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    if let Some(arr) = out.body.get("cases").and_then(|v| v.as_array()) {
        assert!(
            arr.is_empty(),
            "{label} must not include cases: {}",
            out.body
        );
    }
    if let Some(id) = out.body.get("id").and_then(|v| v.as_str()) {
        panic!("{label} must not return a case id {id}: {}", out.body);
    }
    for secret in leaks {
        assert!(
            !out.raw.contains(secret),
            "{label} body must not contain {secret:?}: {}",
            out.raw
        );
        assert!(
            !out.body.to_string().contains(secret),
            "{label} json must not contain {secret:?}: {}",
            out.body
        );
    }
}

/// GET /cases and POST /cases on another workspace must 403 not-a-member
/// and must not leak or insert. Distinct from
/// `cases_other_workspace_cannot_get_observe_decide_cancel_wake` (by-id
/// GET/observe/decide/cancel/wake) and
/// `cases_wake_owner_ok_other_workspace_forbidden` (wake).
#[tokio::test]
async fn cases_list_create_other_ws_are_403_and_do_not_insert() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "A and B must have distinct workspaces");

    let a_secret_title = format!("A-secret-case-title-{}", Uuid::new_v4());
    let a_case_id = create_open_case(&app, &a_jar, &a_ws, &a_secret_title).await;

    let snap = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "A GET /cases: {}", snap.body);
    let snap_rows = listed_case_id_titles(&snap.body);
    let snap_count = snap.body["count"]
        .as_u64()
        .unwrap_or_else(|| snap_rows.len() as u64);
    assert_eq!(
        snap_count as usize,
        snap_rows.len(),
        "A snapshot count must match cases array: {}",
        snap.body
    );
    assert!(
        snap_rows
            .iter()
            .any(|(id, title)| id == &a_case_id && title == &a_secret_title),
        "A snapshot must include planted {a_case_id} / {a_secret_title} so the leak check is not vacuous: {}",
        snap.body
    );

    let list_as_b = call(&app, "GET", &cases_path(&a_ws, ""), Some(&b_jar), None).await;
    assert_cases_not_a_member(
        &list_as_b,
        "B GET A's /cases",
        &[a_case_id.as_str(), a_secret_title.as_str()],
    );

    let b_stolen_title = format!("B-stolen-into-A-{}", Uuid::new_v4());
    let create_as_b = call(
        &app,
        "POST",
        &cases_path(&a_ws, ""),
        Some(&b_jar),
        Some(json!({
            "title": b_stolen_title,
            "policy_md": "must not land",
            "wake_in_s": 3600
        })),
    )
    .await;
    assert_cases_not_a_member(
        &create_as_b,
        "B POST A's /cases",
        &[a_case_id.as_str(), a_secret_title.as_str()],
    );
    assert_ne!(
        create_as_b.status,
        StatusCode::OK,
        "B POST A's /cases must not 200 with a new id: {}",
        create_as_b.body
    );

    let after = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "A GET /cases after B: {}",
        after.body
    );
    let after_rows = listed_case_id_titles(&after.body);
    let after_count = after.body["count"]
        .as_u64()
        .unwrap_or_else(|| after_rows.len() as u64);
    assert_eq!(
        after_count, snap_count,
        "A's list count must still match the snapshot (no extra row): snap={snap_count} after={after_count} body={}",
        after.body
    );
    assert_eq!(
        after_rows, snap_rows,
        "A's list must still match the snapshot (no extra row, original title unchanged): snap={snap_rows:?} after={after_rows:?} body={}",
        after.body
    );
    assert!(
        after_rows
            .iter()
            .all(|(id, title)| { !(id == &a_case_id && title != &a_secret_title) }),
        "original title of {a_case_id} must be unchanged: {}",
        after.body
    );
    assert!(
        !after.raw.contains(&b_stolen_title)
            && after_rows.iter().all(|(_, title)| title != &b_stolen_title),
        "B's stolen title must not appear on A's list: {}",
        after.body
    );

    let b_list = call(&app, "GET", &cases_path(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_list.status,
        StatusCode::OK,
        "B GET B /cases must be 200 so this is not a tautology that every cases call is 403: {}",
        b_list.body
    );
    assert!(
        !b_list.raw.contains(&a_case_id) && !b_list.raw.contains(&a_secret_title),
        "B GET B /cases must not include A's id or title: {}",
        b_list.raw
    );
    let b_rows = listed_case_id_titles(&b_list.body);
    assert!(
        b_rows
            .iter()
            .all(|(id, title)| id != &a_case_id && title != &a_secret_title),
        "B GET B /cases must not list A's case: {}",
        b_list.body
    );

    let b_own_title = format!("B-own-case-{}", Uuid::new_v4());
    let b_own_id = create_open_case(&app, &b_jar, &b_ws, &b_own_title).await;
    let b_got = call(
        &app,
        "GET",
        &cases_path(&b_ws, &format!("/{b_own_id}")),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_got.status,
        StatusCode::OK,
        "control: B GET B /cases/{{id}} must be 200 so this is not a tautology that every cases call is 403: {}",
        b_got.body
    );
    assert_eq!(b_got.body["id"], b_own_id);
    assert_eq!(b_got.body["title"], b_own_title);

    let b_list_after = call(&app, "GET", &cases_path(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_list_after.status,
        StatusCode::OK,
        "control: B GET B /cases after own create: {}",
        b_list_after.body
    );
    let b_after_rows = listed_case_id_titles(&b_list_after.body);
    assert!(
        b_after_rows
            .iter()
            .any(|(id, title)| id == &b_own_id && title == &b_own_title),
        "control: B GET B /cases must include B's own case: {}",
        b_list_after.body
    );
    assert!(
        b_after_rows
            .iter()
            .all(|(id, title)| id != &a_case_id && title != &a_secret_title),
        "control: B's list must still omit A's case: {}",
        b_list_after.body
    );
}

#[tokio::test]
async fn cases_cancel_is_terminal_and_idempotent() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "cancel-fsm").await;

    let cancelled = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/cancel")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["status"], "cancelled");

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
        "observe on terminal: {}",
        observed.body
    );

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
        StatusCode::CONFLICT,
        "decide on cancelled is 409 Conflict, not 400: {}",
        decided.body
    );
    assert_ne!(
        decided.status,
        StatusCode::BAD_REQUEST,
        "a well-formed decide on a cancelled case is a conflict, not invalid input: {}",
        decided.body
    );
    let decide_err = decided.body["error"].as_str().unwrap_or("");
    assert!(
        decide_err.to_ascii_lowercase().contains("terminal"),
        "decide on cancelled 409 must mention terminal: {decide_err:?} body={}",
        decided.body
    );

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
}

/// POST /cancel on acting must be terminal and drop `run_id`. When that
/// fleet run later becomes terminal, `process_due` / `reconcile_acting_runs`
/// must not revive the case or insert a second accepted plan.
#[tokio::test]
async fn cancel_acting_clears_run_and_does_not_relaunch_when_fleet_run_completes() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace id");

    let one_task = json!({
        "decision": {
            "action": "launch_agents",
            "launch": {
                "title": "one-task",
                "tasks": [{"id": "t1", "title": "T1", "spec": "do the work"}]
            }
        }
    });

    let title = format!("cancel-acting-clears-run-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &jar, &ws, &title).await;
    let launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(one_task.clone()),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
    assert_eq!(
        launched.body["status"], "acting",
        "one-task launch_agents must set acting: {}",
        launched.body
    );
    assert_ne!(launched.body["status"], "waiting", "{}", launched.body);
    assert_ne!(launched.body["status"], "deciding", "{}", launched.body);
    assert_ne!(launched.body["status"], "open", "{}", launched.body);
    let run_id = launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("launch must return run_id uuid: {}", launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&run_id).is_ok(),
        "run_id must be a uuid: {run_id:?} body={}",
        launched.body
    );
    let run_uuid = Uuid::parse_str(&run_id).expect("run uuid");

    let got_acting = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_acting.status, StatusCode::OK, "{}", got_acting.body);
    assert_eq!(
        got_acting.body["status"], "acting",
        "GET after launch_agents: {}",
        got_acting.body
    );
    assert_eq!(
        got_acting.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET must persist the same run_id: {}",
        got_acting.body
    );

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);
    assert!(
        snap_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must list the launched run {run_id}: {}",
        snap.body
    );
    assert_eq!(
        snap_ids,
        vec![run_id.clone()],
        "this workspace must have only that first run before cancel: {snap_ids:?} body={}",
        snap.body
    );

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
        cancelled.body["status"], "cancelled",
        "POST /cancel on acting must mark cancelled: {}",
        cancelled.body
    );
    assert_ne!(cancelled.body["status"], "waiting", "{}", cancelled.body);
    assert_ne!(cancelled.body["status"], "acting", "{}", cancelled.body);
    assert_ne!(cancelled.body["status"], "deciding", "{}", cancelled.body);
    assert_ne!(cancelled.body["status"], "open", "{}", cancelled.body);

    let got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["status"], "cancelled",
        "GET after cancel: {}",
        got.body
    );
    assert_ne!(got.body["status"], "waiting", "{}", got.body);
    assert_ne!(got.body["status"], "acting", "{}", got.body);
    assert_ne!(got.body["status"], "deciding", "{}", got.body);
    assert_ne!(got.body["status"], "open", "{}", got.body);
    assert!(
        got.body["run_id"].is_null(),
        "cancel must NULL run_id: {}",
        got.body
    );
    assert!(
        got.body["next_wake_at"].is_null(),
        "cancelled next_wake_at must be null: {}",
        got.body
    );

    let after_cancel_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_cancel_runs.status,
        StatusCode::OK,
        "runs after cancel: {}",
        after_cancel_runs.body
    );
    let after_cancel_ids = fleet_run_ids(&after_cancel_runs.body);
    assert_eq!(
        after_cancel_ids, snap_ids,
        "cancel must not insert a second fleet run: snap={snap_ids:?} after={after_cancel_ids:?} body={}",
        after_cancel_runs.body
    );
    assert_eq!(
        after_cancel_ids,
        vec![run_id.clone()],
        "GET /fleet/runs must still have only that first id: {}",
        after_cancel_runs.body
    );

    state
        .orch
        .store
        .set_run_terminal(run_uuid, "completed")
        .await
        .expect("mark snapshot run terminal");
    let stored = state
        .orch
        .store
        .get_run(workspace_id, run_uuid)
        .await
        .expect("get_run")
        .unwrap_or_else(|| panic!("snapshot run {run_id} must still exist"));
    assert!(
        matches!(stored.status.as_str(), "completed" | "failed" | "cancelled"),
        "get_run must be terminal after UPDATE work_runs: status={:?} id={run_id}",
        stored.status
    );

    fleet_server::cases::process_due(&state, 20).await;

    let after = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["status"], "cancelled",
        "process_due must not revive a cancelled acting-then-cancelled case: {}",
        after.body
    );
    assert_ne!(after.body["status"], "waiting", "{}", after.body);
    assert_ne!(after.body["status"], "acting", "{}", after.body);
    assert_ne!(after.body["status"], "deciding", "{}", after.body);
    assert_ne!(after.body["status"], "open", "{}", after.body);
    assert!(
        after.body["run_id"].is_null(),
        "process_due must not restore run_id: {}",
        after.body
    );
    assert!(
        after.body["next_wake_at"].is_null(),
        "process_due must not schedule a wake on cancelled: {}",
        after.body
    );

    let after_due_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_due_runs.status,
        StatusCode::OK,
        "runs after process_due: {}",
        after_due_runs.body
    );
    let after_due_ids = fleet_run_ids(&after_due_runs.body);
    assert_eq!(
        after_due_ids, snap_ids,
        "process_due must not grow a new accepted plan for this case: snap={snap_ids:?} after={after_due_ids:?} body={}",
        after_due_runs.body
    );
    assert_eq!(
        after_due_ids,
        vec![run_id.clone()],
        "GET /fleet/runs must still be the same run_id only: {}",
        after_due_runs.body
    );

    let ctrl_title = format!("cancel-acting-clears-run-ctrl-{}", Uuid::new_v4());
    let ctrl_id = create_open_case(&app, &jar, &ws, &ctrl_title).await;
    assert_ne!(ctrl_id, case_id, "control must be a second case");
    let ctrl_launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{ctrl_id}/decide")),
        Some(&jar),
        Some(one_task),
    )
    .await;
    assert_eq!(
        ctrl_launched.status,
        StatusCode::OK,
        "{}",
        ctrl_launched.body
    );
    assert_eq!(
        ctrl_launched.body["status"], "acting",
        "control launch_agents must set acting so this is not a tautology that every case is cancelled: {}",
        ctrl_launched.body
    );
    let ctrl_run_id = ctrl_launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("control launch run_id: {}", ctrl_launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&ctrl_run_id).is_ok(),
        "control run_id must be a uuid: {ctrl_run_id:?}"
    );
    assert_ne!(ctrl_run_id, run_id, "control must have its own fleet run");

    let still_cancelled = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        still_cancelled.status,
        StatusCode::OK,
        "{}",
        still_cancelled.body
    );
    assert_eq!(
        still_cancelled.body["status"], "cancelled",
        "launching the control must not revive the cancelled case: {}",
        still_cancelled.body
    );
    assert!(
        still_cancelled.body["run_id"].is_null(),
        "control launch must not restore the cancelled case run_id: {}",
        still_cancelled.body
    );
}

/// POST /decide wait on completed/cancelled/failed is 409 Conflict.
/// Must fail closed: do not reopen, do not bump version/decision_count,
/// do not append kind=decided (or any new event). 400 would treat a legal
/// body as invalid. Control: still-open wait is 200 waiting, decision_count=1.
#[tokio::test]
async fn decide_on_terminal_completed_cancelled_failed_is_409_and_does_not_reopen() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for terminal in ["completed", "cancelled", "failed"] {
        let title = format!("decide-on-terminal-{terminal}");
        let case_id = create_open_case(&app, &jar, &ws, &title).await;
        let seed = match terminal {
            "completed" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/decide")),
                    Some(&jar),
                    Some(json!({ "decision": { "action": "complete" } })),
                )
                .await
            }
            "cancelled" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/cancel")),
                    Some(&jar),
                    Some(json!({})),
                )
                .await
            }
            "failed" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/decide")),
                    Some(&jar),
                    Some(json!({ "decision": { "action": "fail" } })),
                )
                .await
            }
            other => panic!("unexpected terminal {other}"),
        };
        assert_eq!(
            seed.status,
            StatusCode::OK,
            "seed {terminal}: {}",
            seed.body
        );
        assert_eq!(
            seed.body["status"], terminal,
            "seed {terminal}: {}",
            seed.body
        );

        let before = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{case_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            before.status,
            StatusCode::OK,
            "GET {terminal}: {}",
            before.body
        );
        assert_eq!(
            before.body["status"], terminal,
            "GET after seed {terminal}: {}",
            before.body
        );
        let version = before.body["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("version {terminal}: {}", before.body));
        let decision_count = before.body["decision_count"]
            .as_i64()
            .unwrap_or_else(|| panic!("decision_count {terminal}: {}", before.body));
        let events_before = before.body["events"]
            .as_array()
            .unwrap_or_else(|| panic!("events {terminal}: {}", before.body))
            .clone();
        let kinds_before: Vec<&str> = events_before
            .iter()
            .filter_map(|e| e["kind"].as_str())
            .collect();
        let ids_before: Vec<&str> = events_before
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();
        let decided_before = kinds_before.iter().filter(|k| **k == "decided").count();

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
            StatusCode::CONFLICT,
            "POST /decide wait on {terminal} must be 409 Conflict: {}",
            decided.body
        );
        assert_ne!(
            decided.status,
            StatusCode::BAD_REQUEST,
            "well-formed decide on {terminal} is a conflict, not invalid input: {}",
            decided.body
        );
        assert_ne!(
            decided.status,
            StatusCode::OK,
            "POST /decide wait on {terminal} must not 200/reopen: {}",
            decided.body
        );
        let err = decided.body["error"].as_str().unwrap_or("");
        assert!(
            err.to_ascii_lowercase().contains("terminal"),
            "409 body on {terminal} must mention terminal: {err:?} body={}",
            decided.body
        );

        let after = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{case_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "GET after decide {terminal}: {}",
            after.body
        );
        assert_eq!(
            after.body["status"], terminal,
            "GET after refuse must stay {terminal}: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "deciding",
            "decide must not reopen {terminal} into deciding: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "waiting",
            "decide must not reopen {terminal} into waiting: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "open",
            "decide must not reopen {terminal} into open: {}",
            after.body
        );
        assert_eq!(
            after.body["version"].as_i64(),
            Some(version),
            "refused decide must not bump version on {terminal}: {}",
            after.body
        );
        assert_eq!(
            after.body["decision_count"].as_i64(),
            Some(decision_count),
            "refused decide must not bump decision_count on {terminal}: {}",
            after.body
        );

        let events_after = after.body["events"]
            .as_array()
            .unwrap_or_else(|| panic!("events after {terminal}: {}", after.body));
        let kinds_after: Vec<&str> = events_after
            .iter()
            .filter_map(|e| e["kind"].as_str())
            .collect();
        let ids_after: Vec<&str> = events_after
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();
        assert_eq!(
            events_after.len(),
            events_before.len(),
            "failed decide on {terminal} must not append any event; before={kinds_before:?} after={kinds_after:?}"
        );
        assert_eq!(
            ids_after, ids_before,
            "failed decide on {terminal} must not replace or grow events: before={ids_before:?} after={ids_after:?}"
        );
        assert_eq!(
            kinds_after, kinds_before,
            "failed decide on {terminal} must keep the same event kinds"
        );
        let decided_after = kinds_after.iter().filter(|k| **k == "decided").count();
        assert_eq!(
            decided_after, decided_before,
            "failed decide on {terminal} must not grow kind=decided; before={kinds_before:?} after={kinds_after:?}"
        );
    }

    // Control: a still-open case can wait so this is not a tautology that
    // every POST /decide is 409.
    let live_id = create_open_case(&app, &jar, &ws, "decide-on-terminal-live").await;
    let live = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{live_id}/decide")),
        Some(&jar),
        Some(json!({ "decision": { "action": "wait" } })),
    )
    .await;
    assert_eq!(
        live.status,
        StatusCode::OK,
        "POST /decide wait on an open case must still be 200: {}",
        live.body
    );
    assert_eq!(
        live.body["status"], "waiting",
        "open decide wait must enter waiting: {}",
        live.body
    );
    assert_eq!(
        live.body["decision_count"].as_i64(),
        Some(1),
        "open decide wait must bump decision_count to 1: {}",
        live.body
    );
}

/// `force_complete` is a decide-time hint, not a status override. Cancel
/// keeps the flag; a planted leftover due wake must not let `process_due`
/// complete that row. An open sibling with the same flag must complete so
/// this is not a tautology that the wake loop never completes. Grok/Ollama
/// stay off in this test app so `decide_for_case` is the heuristic.
#[tokio::test]
async fn heuristic_force_complete_process_due_skips_cancelled_leftover_flag() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace id");

    let leftover_title = format!("heuristic-force-complete-cancelled-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": leftover_title,
            "policy_md": "test policy",
            "context": { "force_complete": true },
            "wake_in_s": 0
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(
        created.body["context"]["force_complete"], true,
        "create must persist context.force_complete: {}",
        created.body
    );
    assert_eq!(created.body["title"], leftover_title, "{}", created.body);
    let case_id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case id: {}", created.body))
        .to_string();
    let case_uuid = Uuid::parse_str(&case_id).expect("case id");

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
        cancelled.body["status"], "cancelled",
        "POST /cancel must mark cancelled: {}",
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
    assert_eq!(
        got.body["status"], "cancelled",
        "GET after cancel: {}",
        got.body
    );
    assert_eq!(
        got.body["context"]["force_complete"], true,
        "cancel must keep context.force_complete; clearing the flag would hide the hole: {}",
        got.body
    );
    assert!(
        got.body["next_wake_at"].is_null(),
        "cancelled next_wake_at must be null before the leftover plant: {}",
        got.body
    );
    let version = got.body["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("version: {}", got.body));
    let decision_count = got.body["decision_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("decision_count: {}", got.body));
    let events_before = got.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got.body))
        .clone();
    let decided_before = events_before
        .iter()
        .filter(|e| e["kind"].as_str() == Some("decided"))
        .count();
    let completed_before = events_before
        .iter()
        .filter(|e| e["kind"].as_str() == Some("completed"))
        .count();

    let leftover = chrono::Utc::now() - chrono::Duration::seconds(30);
    let planted = sqlx::query(
        r#"
        UPDATE cases
        SET next_wake_at = $3
        WHERE workspace_id = $1 AND id = $2 AND status = 'cancelled'
        "#,
    )
    .bind(workspace_id)
    .bind(case_uuid)
    .bind(leftover)
    .execute(&state.db)
    .await
    .expect("plant leftover due wake on cancelled row");
    assert_eq!(
        planted.rows_affected(),
        1,
        "expected the cancelled leftover-flag row planted with a due wake: {case_id}"
    );

    let planted_got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(planted_got.status, StatusCode::OK, "{}", planted_got.body);
    assert_eq!(
        planted_got.body["status"], "cancelled",
        "planted leftover wake must keep cancelled: {}",
        planted_got.body
    );
    assert_eq!(
        planted_got.body["context"]["force_complete"], true,
        "plant must keep leftover force_complete: {}",
        planted_got.body
    );
    let planted_wake = parse_next_wake_at(&planted_got.body);
    assert!(
        planted_wake <= chrono::Utc::now(),
        "leftover wake must be due so a status-blind force_complete scan would see it: {planted_wake} body={}",
        planted_got.body
    );
    assert_eq!(
        planted_got.body["version"].as_i64(),
        Some(version),
        "plant must not bump version: {}",
        planted_got.body
    );

    let ctrl_title = format!("heuristic-force-complete-open-{}", Uuid::new_v4());
    let ctrl = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": ctrl_title,
            "policy_md": "test policy",
            "context": { "force_complete": true },
            "wake_in_s": 0
        })),
    )
    .await;
    assert_eq!(ctrl.status, StatusCode::OK, "{}", ctrl.body);
    assert_eq!(
        ctrl.body["context"]["force_complete"], true,
        "control must persist context.force_complete: {}",
        ctrl.body
    );
    assert_ne!(ctrl.body["status"], "cancelled", "{}", ctrl.body);
    assert_ne!(ctrl.body["status"], "completed", "{}", ctrl.body);
    let ctrl_id = ctrl.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control id: {}", ctrl.body))
        .to_string();
    assert_ne!(ctrl_id, case_id, "control must be a second case");
    let ctrl_uuid = Uuid::parse_str(&ctrl_id).expect("control id");

    // Oldest possible wake so other due rows cannot sort ahead (isolation=none).
    let ctrl_planted = sqlx::query(
        r#"
        UPDATE cases
        SET next_wake_at = $3
        WHERE workspace_id = $1 AND id = $2 AND status IN ('open', 'waiting')
        "#,
    )
    .bind(workspace_id)
    .bind(ctrl_uuid)
    .bind(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
    .execute(&state.db)
    .await
    .expect("plant oldest due wake on open control");
    assert_eq!(
        ctrl_planted.rows_affected(),
        1,
        "expected the still-open control planted with a due wake: {ctrl_id}"
    );

    fleet_server::cases::process_due(&state, 20).await;

    let after = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(
        after.body["status"], "cancelled",
        "process_due must not complete a cancelled leftover-flag row: {}",
        after.body
    );
    assert_ne!(after.body["status"], "completed", "{}", after.body);
    assert_ne!(after.body["status"], "deciding", "{}", after.body);
    assert_ne!(after.body["status"], "waiting", "{}", after.body);
    assert_ne!(after.body["status"], "open", "{}", after.body);
    assert_eq!(
        after.body["version"].as_i64(),
        Some(version),
        "process_due must not bump version on the cancelled leftover-flag row: {}",
        after.body
    );
    assert_eq!(
        after.body["decision_count"].as_i64(),
        Some(decision_count),
        "process_due must not bump decision_count on the cancelled leftover-flag row: {}",
        after.body
    );
    assert_eq!(
        after.body["context"]["force_complete"], true,
        "leftover force_complete must still be present after process_due: {}",
        after.body
    );
    let events_after = after.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events after process_due: {}", after.body));
    let decided_after = events_after
        .iter()
        .filter(|e| e["kind"].as_str() == Some("decided"))
        .count();
    let completed_after = events_after
        .iter()
        .filter(|e| e["kind"].as_str() == Some("completed"))
        .count();
    assert_eq!(
        decided_after, decided_before,
        "events must not grow kind=decided on the cancelled leftover-flag row: before={decided_before} after={decided_after} body={}",
        after.body
    );
    assert_eq!(
        completed_after, completed_before,
        "events must not grow kind=completed on the cancelled leftover-flag row: before={completed_before} after={completed_after} body={}",
        after.body
    );

    let mut ctrl_got = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{ctrl_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(ctrl_got.status, StatusCode::OK, "{}", ctrl_got.body);
    // Other due rows may fill one claim_due batch (shared DB). process_due
    // must still complete this open force_complete case; leftover stays cancelled.
    for _ in 0..7 {
        if ctrl_got.body["status"] == "completed" {
            break;
        }
        fleet_server::cases::process_due(&state, 20).await;
        let leftover_again = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{case_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            leftover_again.body["status"], "cancelled",
            "retry process_due must still leave the leftover-flag row cancelled: {}",
            leftover_again.body
        );
        ctrl_got = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{ctrl_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(ctrl_got.status, StatusCode::OK, "{}", ctrl_got.body);
    }
    assert_eq!(
        ctrl_got.body["status"], "completed",
        "control: still-open force_complete + due wake must complete via process_due \
         so this is not a tautology that process_due never completes: {}",
        ctrl_got.body
    );
    assert_ne!(ctrl_got.body["status"], "cancelled", "{}", ctrl_got.body);
    assert_ne!(ctrl_got.body["status"], "deciding", "{}", ctrl_got.body);
    assert_ne!(ctrl_got.body["status"], "waiting", "{}", ctrl_got.body);
    assert_ne!(ctrl_got.body["status"], "open", "{}", ctrl_got.body);
}

#[tokio::test]
async fn cases_observe_empty_note_is_required() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "observe-note").await;

    for note in ["", "   ", "\t\n"] {
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
        let err = out.body["error"].as_str().unwrap_or("");
        assert!(
            err.contains("note is required"),
            "expected 'note is required', got {err:?} body={}",
            out.body
        );
    }
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

/// POST /cases `wake_in_s` is the first-wake schedule (`create_case`).
/// Huge values must not overflow `chrono::Duration::seconds` (5xx / panic).
/// 400 or 200 with `next_wake_at` within `now + MAX_WAIT_SECS` is the lock.
/// Negative must not schedule a past wake (ticker spin). Decide wait stays
/// `.clamp(5, MAX_WAIT_SECS)` — do not weaken `cases_wait_seconds_huge_clamps_to_max`.
#[tokio::test]
async fn cases_wake_in_s_huge_or_negative_does_not_overflow() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let max_wait = fleet_server::cases::MAX_WAIT_SECS;
    let huge: i64 = 1_000_000_000_000; // 10^12 — overflows chrono if unclamped

    let list_before = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        list_before.status,
        StatusCode::OK,
        "list before huge create: {}",
        list_before.body
    );
    let count_before = list_before.body["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("count: {}", list_before.body));
    let huge_title = format!("wake-in-huge-{}", Uuid::new_v4());

    let t0 = chrono::Utc::now();
    let created = call(
        &app,
        "POST",
        &cases_path(&ws, ""),
        Some(&jar),
        Some(json!({
            "title": huge_title,
            "wake_in_s": huge
        })),
    )
    .await;
    let t1 = chrono::Utc::now();
    assert!(
        !created.status.is_server_error(),
        "huge wake_in_s must not 5xx / panic Duration::seconds: {} {}",
        created.status,
        created.body
    );
    assert!(
        created.status == StatusCode::OK || created.status == StatusCode::BAD_REQUEST,
        "huge wake_in_s must be 200 (clamped) or 400, not {}: {}",
        created.status,
        created.body
    );

    let listed = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let listed_ids: Vec<&str> = listed.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", listed.body))
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    let listed_titles: Vec<&str> = listed.body["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("cases: {}", listed.body))
        .iter()
        .filter_map(|c| c["title"].as_str())
        .collect();
    let count_after = listed.body["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("count: {}", listed.body));

    if created.status == StatusCode::OK {
        let case_id = created.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case id: {}", created.body));
        let latest = t1 + chrono::Duration::seconds(max_wait + 2);
        let next = parse_create_wake_within_max(&created.body, t0, latest, "POST /cases 10^12");
        assert!(
            next <= t1 + chrono::Duration::days(8),
            "clamped create wake must be days away, not 10^12s: next={next} body={}",
            created.body
        );
        assert!(
            listed_ids.contains(&case_id),
            "GET /cases after 200 must include {case_id}: {}",
            listed.body
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
        let _persisted =
            parse_create_wake_within_max(&got.body, t0, latest, "GET /cases/{id} after 10^12");
    } else {
        assert_eq!(
            count_after, count_before,
            "400 huge wake_in_s must not insert a row: before={count_before} after={count_after} body={}",
            listed.body
        );
        assert!(
            !listed_titles.contains(&huge_title.as_str()),
            "rejected huge wake_in_s must not appear on GET /cases: {}",
            listed.body
        );
    }

    for wake in [(-1_i64, "neg-one"), (i64::MIN, "i64-min")] {
        let title = format!("wake-in-{}-{}", wake.1, Uuid::new_v4());
        let before = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
        assert_eq!(before.status, StatusCode::OK, "{}", before.body);
        let n_before = before.body["count"]
            .as_u64()
            .unwrap_or_else(|| panic!("count: {}", before.body));

        let t_neg0 = chrono::Utc::now();
        let out = call(
            &app,
            "POST",
            &cases_path(&ws, ""),
            Some(&jar),
            Some(json!({
                "title": title,
                "wake_in_s": wake.0
            })),
        )
        .await;
        assert!(
            !out.status.is_server_error(),
            "wake_in_s={} must not 5xx / panic Duration::seconds: {} {}",
            wake.0,
            out.status,
            out.body
        );
        assert!(
            out.status == StatusCode::OK || out.status == StatusCode::BAD_REQUEST,
            "wake_in_s={} must be 200 or 400, not {}: {}",
            wake.0,
            out.status,
            out.body
        );

        let after = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        let n_after = after.body["count"]
            .as_u64()
            .unwrap_or_else(|| panic!("count: {}", after.body));
        let after_ids: Vec<&str> = after.body["cases"]
            .as_array()
            .unwrap_or_else(|| panic!("cases: {}", after.body))
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();

        if out.status == StatusCode::OK {
            let next = parse_next_wake_at(&out.body);
            assert!(
                next >= t_neg0,
                "wake_in_s={} must not schedule a past wake (ticker spin): next={next} now={t_neg0} body={}",
                wake.0,
                out.body
            );
            let case_id = out.body["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case id: {}", out.body));
            assert!(
                after_ids.contains(&case_id),
                "GET /cases after 200 must include {case_id}: {}",
                after.body
            );
        } else {
            assert_eq!(
                n_after, n_before,
                "400 wake_in_s={} must not insert: before={n_before} after={n_after} body={}",
                wake.0, after.body
            );
        }
    }

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let tool_title = format!("wake-in-tool-{}", Uuid::new_v4());
    let t_tool0 = chrono::Utc::now();
    let (tool_content, tool_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "start_case",
        &json!({
            "title": tool_title,
            "policy_md": "p",
            "wake_in_s": huge
        }),
        &fleet_server::chat_tools::ChatLlmContext {
            runner_id: None,
            effort: None,
            provider: "grok".into(),
            model: "grok-4.5".into(),
        },
    )
    .await;
    let t_tool1 = chrono::Utc::now();
    if tool_action.ok {
        let latest = t_tool1 + chrono::Duration::seconds(max_wait + 2);
        let _next = parse_create_wake_within_max(
            &tool_content,
            t_tool0,
            latest,
            "start_case 10^12 (same create_case)",
        );
    } else {
        let err = tool_content["error"]
            .as_str()
            .unwrap_or(tool_action.summary.as_str());
        assert!(
            !err.to_ascii_lowercase().contains("internal") && !err.contains("Duration::seconds"),
            "start_case huge wake_in_s must fail closed (400), not overflow: action={tool_action:?} content={tool_content}"
        );
    }

    for (label, body) in [
        (
            "omitted",
            json!({ "title": format!("wake-in-omit-{}", Uuid::new_v4()) }),
        ),
        (
            "zero",
            json!({
                "title": format!("wake-in-zero-{}", Uuid::new_v4()),
                "wake_in_s": 0
            }),
        ),
    ] {
        let out = call(&app, "POST", &cases_path(&ws, ""), Some(&jar), Some(body)).await;
        assert_eq!(
            out.status,
            StatusCode::OK,
            "wake_in_s {label} may 200 (immediate/soon wake is OK on create): {}",
            out.body
        );
        let case_id = out.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case id ({label}): {}", out.body))
            .to_string();
        let listed = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        let ids: Vec<&str> = listed.body["cases"]
            .as_array()
            .unwrap_or_else(|| panic!("cases: {}", listed.body))
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();
        assert!(
            ids.contains(&case_id.as_str()),
            "control {label} must appear on GET /cases: {}",
            listed.body
        );
    }

    let decide_id = create_open_case(&app, &jar, &ws, "wake-in-decide-control").await;
    let t_dec0 = chrono::Utc::now();
    let decided = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{decide_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "wait",
                "wait_seconds": huge,
                "reason": "create wake_in lock must not weaken decide clamp"
            }
        })),
    )
    .await;
    let t_dec1 = chrono::Utc::now();
    assert_eq!(
        decided.status,
        StatusCode::OK,
        "POST decide wait_seconds huge still clamps: {}",
        decided.body
    );
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    let next = parse_next_wake_at(&decided.body);
    let min = t_dec0 + chrono::Duration::seconds(max_wait);
    let max = t_dec1 + chrono::Duration::seconds(max_wait + 2);
    assert!(
        next >= min,
        "decide 10^12 must still clamp to MAX_WAIT_SECS, not collapse: next={next} min={min} body={}",
        decided.body
    );
    assert!(
        next <= max,
        "decide 10^12 must still clamp (no overflow): next={next} max={max} body={}",
        decided.body
    );
}

#[tokio::test]
async fn cases_wake_owner_ok_other_workspace_forbidden() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &a_jar, &a_ws, "wake-owner").await;

    let owner = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{case_id}/wake")),
        Some(&a_jar),
        Some(json!({})),
    )
    .await;
    assert_ne!(
        owner.status,
        StatusCode::NOT_FOUND,
        "owner wake must not 404: {}",
        owner.body
    );
    assert!(
        owner.status == StatusCode::OK || owner.status == StatusCode::BAD_REQUEST,
        "owner wake should be 200 or 400 if engine refuses: {} {}",
        owner.status,
        owner.body
    );

    let other = call(
        &app,
        "POST",
        &cases_path(&a_ws, &format!("/{case_id}/wake")),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert!(
        other.status == StatusCode::FORBIDDEN || other.status == StatusCode::NOT_FOUND,
        "other workspace member must not wake: {} {}",
        other.status,
        other.body
    );
}

/// POST /wake is the UI Wake-now button (`decide_for_case` + `apply_decision`).
/// Terminals (cancelled / completed / failed) 400 and must not reopen or
/// insert a plan. Acting is owned by `reconcile_acting_runs` — HTTP wake
/// must not drop `run_id`, reset to open/waiting, or launch a second plan.
/// Open / waiting still run the engine so this is not "every /wake 400s".
#[tokio::test]
async fn cases_wake_now_terminal_is_400_and_acting_does_not_drop_run() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for terminal in ["cancelled", "completed", "failed"] {
        let title = format!("wake-now-terminal-{terminal}-{}", Uuid::new_v4());
        let case_id = create_open_case(&app, &jar, &ws, &title).await;
        let seed = match terminal {
            "cancelled" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/cancel")),
                    Some(&jar),
                    Some(json!({})),
                )
                .await
            }
            "completed" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/decide")),
                    Some(&jar),
                    Some(json!({ "decision": { "action": "complete" } })),
                )
                .await
            }
            "failed" => {
                call(
                    &app,
                    "POST",
                    &cases_path(&ws, &format!("/{case_id}/decide")),
                    Some(&jar),
                    Some(json!({ "decision": { "action": "fail" } })),
                )
                .await
            }
            other => panic!("unexpected terminal {other}"),
        };
        assert_eq!(
            seed.status,
            StatusCode::OK,
            "seed {terminal}: {}",
            seed.body
        );
        assert_eq!(
            seed.body["status"], terminal,
            "seed {terminal}: {}",
            seed.body
        );

        let before = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{case_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            before.status,
            StatusCode::OK,
            "GET {terminal}: {}",
            before.body
        );
        assert_eq!(
            before.body["status"], terminal,
            "GET after seed {terminal}: {}",
            before.body
        );
        let version = before.body["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("version {terminal}: {}", before.body));
        let decision_count = before.body["decision_count"]
            .as_i64()
            .unwrap_or_else(|| panic!("decision_count {terminal}: {}", before.body));
        let run_id_before = before.body["run_id"].clone();
        let events_before = before.body["events"]
            .as_array()
            .unwrap_or_else(|| panic!("events {terminal}: {}", before.body))
            .clone();
        let ids_before: Vec<&str> = events_before
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();

        let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
        assert_eq!(
            snap.status,
            StatusCode::OK,
            "snapshot runs {terminal}: {}",
            snap.body
        );
        let snap_ids = fleet_run_ids(&snap.body);

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
            "POST /wake on {terminal} must be 400 (case is terminal): {}",
            woke.body
        );
        assert_ne!(
            woke.status,
            StatusCode::OK,
            "POST /wake on {terminal} must not 200/reopen: {}",
            woke.body
        );
        let err = woke.body["error"].as_str().unwrap_or("");
        assert!(
            err.to_ascii_lowercase().contains("terminal"),
            "400 body on {terminal} must mention terminal: {err:?} body={}",
            woke.body
        );

        let after = call(
            &app,
            "GET",
            &cases_path(&ws, &format!("/{case_id}")),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "GET after wake {terminal}: {}",
            after.body
        );
        assert_eq!(
            after.body["status"], terminal,
            "GET after refuse must stay {terminal}: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "open",
            "wake must not reopen {terminal} into open: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "waiting",
            "wake must not reopen {terminal} into waiting: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "deciding",
            "wake must not reopen {terminal} into deciding: {}",
            after.body
        );
        assert_ne!(
            after.body["status"], "acting",
            "wake must not reopen {terminal} into acting: {}",
            after.body
        );
        assert_eq!(
            after.body["version"].as_i64(),
            Some(version),
            "refused wake must not bump version on {terminal}: {}",
            after.body
        );
        assert_eq!(
            after.body["decision_count"].as_i64(),
            Some(decision_count),
            "refused wake must not bump decision_count on {terminal}: {}",
            after.body
        );
        assert_eq!(
            after.body["run_id"], run_id_before,
            "refused wake must not change run_id on {terminal}: {}",
            after.body
        );

        let events_after = after.body["events"]
            .as_array()
            .unwrap_or_else(|| panic!("events after {terminal}: {}", after.body));
        let ids_after: Vec<&str> = events_after
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();
        assert_eq!(
            events_after.len(),
            events_before.len(),
            "refused wake must not append events on {terminal}: before={events_before:?} after={events_after:?}"
        );
        assert_eq!(
            ids_after, ids_before,
            "refused wake must not insert wake_requested / decided on {terminal}: before={ids_before:?} after={ids_after:?}"
        );

        let after_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
        assert_eq!(
            after_runs.status,
            StatusCode::OK,
            "runs after wake {terminal}: {}",
            after_runs.body
        );
        let after_ids = fleet_run_ids(&after_runs.body);
        assert_eq!(
            after_ids, snap_ids,
            "wake on {terminal} must not insert an accepted plan: snap={snap_ids:?} after={after_ids:?} body={}",
            after_runs.body
        );
    }

    let one_task = json!({
        "decision": {
            "action": "launch_agents",
            "launch": {
                "title": "one-task",
                "tasks": [{"id": "t1", "title": "T1", "spec": "do the work"}]
            }
        }
    });
    let acting_title = format!("wake-now-acting-{}", Uuid::new_v4());
    let acting_id = create_open_case(&app, &jar, &ws, &acting_title).await;
    let launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{acting_id}/decide")),
        Some(&jar),
        Some(one_task),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
    assert_eq!(
        launched.body["status"], "acting",
        "one-task launch_agents must set acting: {}",
        launched.body
    );
    assert_ne!(launched.body["status"], "waiting", "{}", launched.body);
    assert_ne!(launched.body["status"], "deciding", "{}", launched.body);
    assert_ne!(launched.body["status"], "open", "{}", launched.body);
    let run_id = launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("launch must return run_id uuid: {}", launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&run_id).is_ok(),
        "run_id must be a uuid: {run_id:?} body={}",
        launched.body
    );

    let got_acting = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{acting_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_acting.status, StatusCode::OK, "{}", got_acting.body);
    assert_eq!(
        got_acting.body["status"], "acting",
        "GET after launch_agents: {}",
        got_acting.body
    );
    assert_eq!(
        got_acting.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET must persist the same run_id: {}",
        got_acting.body
    );

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);
    assert!(
        snap_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must list the launched run {run_id}: {}",
        snap.body
    );
    assert_eq!(
        snap_ids,
        vec![run_id.clone()],
        "this workspace must have only that first run before wake: {snap_ids:?} body={}",
        snap.body
    );

    let woke_acting = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{acting_id}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert!(
        woke_acting.status == StatusCode::OK
            || woke_acting.status == StatusCode::BAD_REQUEST
            || woke_acting.status == StatusCode::CONFLICT,
        "wake on acting must be 200 (keep acting) or 400/409 (run still in progress): {} {}",
        woke_acting.status,
        woke_acting.body
    );
    if woke_acting.status == StatusCode::OK {
        assert_eq!(
            woke_acting.body["status"], "acting",
            "POST /wake on acting must keep acting (reconcile owns the run): {}",
            woke_acting.body
        );
        assert_ne!(
            woke_acting.body["status"], "waiting",
            "wake must not drop acting → waiting while the fleet run is live: {}",
            woke_acting.body
        );
        assert_ne!(
            woke_acting.body["status"], "open",
            "wake must not reset acting → open: {}",
            woke_acting.body
        );
        assert_ne!(
            woke_acting.body["status"], "deciding",
            "wake must not move acting → deciding: {}",
            woke_acting.body
        );
        assert_ne!(
            woke_acting.body["status"], "cancelled",
            "{}",
            woke_acting.body
        );
        assert_ne!(
            woke_acting.body["status"], "completed",
            "{}",
            woke_acting.body
        );
        assert_ne!(woke_acting.body["status"], "failed", "{}", woke_acting.body);
        assert_eq!(
            woke_acting.body["run_id"].as_str(),
            Some(run_id.as_str()),
            "wake must not replace run_id: {}",
            woke_acting.body
        );
    }

    let got_after_wake = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{acting_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got_after_wake.status,
        StatusCode::OK,
        "{}",
        got_after_wake.body
    );
    assert_eq!(
        got_after_wake.body["status"], "acting",
        "GET after wake must stay acting: {}",
        got_after_wake.body
    );
    assert_ne!(
        got_after_wake.body["status"], "waiting",
        "GET must not show waiting after wake-on-acting: {}",
        got_after_wake.body
    );
    assert_ne!(
        got_after_wake.body["status"], "open",
        "GET must not show open after wake-on-acting: {}",
        got_after_wake.body
    );
    assert_ne!(
        got_after_wake.body["status"], "deciding",
        "{}",
        got_after_wake.body
    );
    assert_eq!(
        got_after_wake.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET after wake must keep the original run_id: {}",
        got_after_wake.body
    );

    let after_acting_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_acting_runs.status,
        StatusCode::OK,
        "runs after acting wake: {}",
        after_acting_runs.body
    );
    let after_acting_ids = fleet_run_ids(&after_acting_runs.body);
    assert!(
        after_acting_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must still list first id {run_id}: {}",
        after_acting_runs.body
    );
    assert_eq!(
        after_acting_ids, snap_ids,
        "wake on acting must not insert a second accepted plan: snap={snap_ids:?} after={after_acting_ids:?} body={}",
        after_acting_runs.body
    );
    assert_eq!(
        after_acting_ids,
        vec![run_id.clone()],
        "GET /fleet/runs must still have only that first id: {}",
        after_acting_runs.body
    );

    let open_title = format!("wake-now-open-ctrl-{}", Uuid::new_v4());
    let open_id = create_open_case(&app, &jar, &ws, &open_title).await;
    let woke_open = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{open_id}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_ne!(
        woke_open.status,
        StatusCode::NOT_FOUND,
        "owner wake on open must not 404: {}",
        woke_open.body
    );
    assert!(
        woke_open.status == StatusCode::OK || woke_open.status == StatusCode::BAD_REQUEST,
        "wake on a still-open case must be 200 or 400 if the engine refuses: {} {}",
        woke_open.status,
        woke_open.body
    );
    assert_ne!(
        woke_open.status,
        StatusCode::CONFLICT,
        "open is not terminal; wake must not 409 like decide-on-terminal: {}",
        woke_open.body
    );

    let wait_title = format!("wake-now-waiting-ctrl-{}", Uuid::new_v4());
    let wait_id = create_open_case(&app, &jar, &ws, &wait_title).await;
    let waited = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{wait_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "wait", "wait_seconds": 3600 }
        })),
    )
    .await;
    assert_eq!(waited.status, StatusCode::OK, "{}", waited.body);
    assert_eq!(
        waited.body["status"], "waiting",
        "control decide wait must enter waiting: {}",
        waited.body
    );
    let woke_waiting = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{wait_id}/wake")),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_ne!(
        woke_waiting.status,
        StatusCode::NOT_FOUND,
        "owner wake on waiting must not 404: {}",
        woke_waiting.body
    );
    assert!(
        woke_waiting.status == StatusCode::OK || woke_waiting.status == StatusCode::BAD_REQUEST,
        "wake on waiting must still run the engine (200 or 400 if it refuses): {} {}",
        woke_waiting.status,
        woke_waiting.body
    );
    assert_ne!(
        woke_waiting.status,
        StatusCode::CONFLICT,
        "waiting is not terminal; wake must not 409: {}",
        woke_waiting.body
    );
}

#[tokio::test]
async fn cases_decide_unknown_action_waits_with_last_error() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let case_id = create_open_case(&app, &jar, &ws, "unknown-action").await;

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
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
    assert_eq!(decided.body["status"], "waiting", "{}", decided.body);
    let err = decided.body["last_error"].as_str().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("unknown action"),
        "last_error must mention unknown action: {err:?} body={}",
        decided.body
    );
}

/// POST /decide launch_agents writes acting only with the inserted fleet run.
/// A rejected plan (cycle / dup id / unknown dep) waits with no run_id and
/// does not insert a work_run. Acting with no run is permanently stuck.
#[tokio::test]
async fn launch_agents_decide_sets_acting_with_run_id_and_bad_plan_does_not_act_without_run() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let snap = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "snapshot runs: {}", snap.body);
    let snap_ids = fleet_run_ids(&snap.body);

    let ok_title = format!("launch-ok-{}", Uuid::new_v4());
    let ok_id = create_open_case(&app, &jar, &ws, &ok_title).await;
    let launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{ok_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": {
                "action": "launch_agents",
                "launch": { "title": "dark", "spec": "do it" }
            }
        })),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
    assert_eq!(
        launched.body["status"], "acting",
        "good launch_agents must set acting: {}",
        launched.body
    );
    assert_ne!(launched.body["status"], "waiting", "{}", launched.body);
    assert_ne!(launched.body["status"], "deciding", "{}", launched.body);
    assert_ne!(launched.body["status"], "open", "{}", launched.body);
    let run_id = launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("good launch must return run_id uuid: {}", launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&run_id).is_ok(),
        "run_id must be a uuid: {run_id:?} body={}",
        launched.body
    );
    assert!(
        launched.body["next_wake_at"].is_null(),
        "acting is owned by reconcile, not claim_due: {}",
        launched.body
    );
    assert!(
        launched.body["last_error"].is_null(),
        "good launch must clear last_error: {}",
        launched.body
    );

    let got_ok = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{ok_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_ok.status, StatusCode::OK, "{}", got_ok.body);
    assert_eq!(got_ok.body["status"], "acting", "{}", got_ok.body);
    assert_eq!(
        got_ok.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "GET must persist the same run_id: {}",
        got_ok.body
    );
    assert!(
        !got_ok.body["run_id"].is_null(),
        "must not be waiting/deciding/open with a null run_id: {}",
        got_ok.body
    );
    let ok_events = got_ok.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got_ok.body));
    let launched_ev: Vec<&Value> = ok_events
        .iter()
        .filter(|e| e["kind"].as_str() == Some("agents_launched"))
        .collect();
    assert!(
        !launched_ev.is_empty(),
        "events must include agents_launched: {}",
        got_ok.body
    );
    assert!(
        launched_ev
            .iter()
            .any(|e| e["payload"]["run_id"].as_str() == Some(run_id.as_str())),
        "agents_launched payload run_id must match {run_id}: {}",
        got_ok.body
    );

    let after_good = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        after_good.status,
        StatusCode::OK,
        "runs after good launch: {}",
        after_good.body
    );
    let after_good_ids = fleet_run_ids(&after_good.body);
    assert!(
        after_good_ids.iter().any(|id| id == &run_id),
        "GET /fleet/runs must list the new accepted plan {run_id}: snap={snap_ids:?} after={}",
        after_good.body
    );
    assert_eq!(
        after_good_ids.len(),
        snap_ids.len() + 1,
        "good launch must insert exactly one new fleet run: snap={snap_ids:?} after={after_good_ids:?}"
    );

    let bad_plans = [
        (
            "cycle",
            json!({
                "tasks": [
                    {"id": "a", "title": "A", "depends_on": ["b"]},
                    {"id": "b", "title": "B", "depends_on": ["a"]}
                ]
            }),
            "cycle",
        ),
        (
            "dup",
            json!({
                "tasks": [
                    {"id": "t1", "title": "A"},
                    {"id": "t1", "title": "B"}
                ]
            }),
            "duplicate",
        ),
        (
            "unknown-dep",
            json!({
                "tasks": [{"id": "t1", "title": "A", "depends_on": ["missing"]}]
            }),
            "depends_on",
        ),
    ];
    for (label, launch, needle) in bad_plans {
        let title = format!("launch-{label}-{}", Uuid::new_v4());
        let case_id = create_open_case(&app, &jar, &ws, &title).await;
        let decided = call(
            &app,
            "POST",
            &cases_path(&ws, &format!("/{case_id}/decide")),
            Some(&jar),
            Some(json!({
                "decision": { "action": "launch_agents", "launch": launch }
            })),
        )
        .await;
        assert_eq!(decided.status, StatusCode::OK, "{label}: {}", decided.body);
        assert_eq!(
            decided.body["status"], "waiting",
            "bad plan {label} must wait, not act: {}",
            decided.body
        );
        assert_ne!(
            decided.body["status"], "acting",
            "a bad plan must not leave the case acting with no run ({label}): {}",
            decided.body
        );
        assert_ne!(
            decided.body["status"], "deciding",
            "bad plan {label} must not leave deciding: {}",
            decided.body
        );
        assert!(
            decided.body["run_id"].is_null(),
            "failed launch {label} must not invent a run_id: {}",
            decided.body
        );
        let err = decided.body["last_error"].as_str().unwrap_or("");
        let err_l = err.to_ascii_lowercase();
        assert!(
            err_l.contains(needle) || err_l.contains("unknown"),
            "last_error for {label} must mention {needle:?} (or unknown): {err:?} body={}",
            decided.body
        );
        let next = parse_next_wake_at(&decided.body);
        let now = chrono::Utc::now();
        assert!(
            next > now - chrono::Duration::seconds(2),
            "failed launch {label} next_wake_at must be a retry, not the past: {next} body={}",
            decided.body
        );
        assert!(
            next <= now + chrono::Duration::seconds(90),
            "failed launch {label} retry must be soon (~60s), not the create-time 3600s wake: {next} body={}",
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
        assert_eq!(got.status, StatusCode::OK, "{label} get: {}", got.body);
        assert_eq!(got.body["status"], "waiting", "{label} get: {}", got.body);
        assert!(
            got.body["run_id"].is_null(),
            "GET after bad {label} must not leave acting with no run: {}",
            got.body
        );
        let kinds: Vec<&str> = got.body["events"]
            .as_array()
            .unwrap_or_else(|| panic!("{label} events: {}", got.body))
            .iter()
            .filter_map(|e| e["kind"].as_str())
            .collect();
        assert!(
            kinds.contains(&"agents_launch_failed"),
            "events for {label} must include agents_launch_failed: {kinds:?} body={}",
            got.body
        );
        assert!(
            !kinds.contains(&"agents_launched"),
            "events for {label} must not include a successful agents_launched: {kinds:?} body={}",
            got.body
        );

        let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
        assert_eq!(runs.status, StatusCode::OK, "{label} runs: {}", runs.body);
        assert_eq!(
            fleet_run_ids(&runs.body),
            after_good_ids,
            "bad plan {label} must not grow a new accepted plan: after_good={after_good_ids:?} now={}",
            runs.body
        );
    }

    let still_ok = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{ok_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(still_ok.status, StatusCode::OK, "{}", still_ok.body);
    assert_eq!(
        still_ok.body["status"], "acting",
        "control: the good launch must still be acting so bad-plan waiting is not a tautology: {}",
        still_ok.body
    );
    assert_eq!(
        still_ok.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "control good launch run_id: {}",
        still_ok.body
    );

    let wait_title = format!("launch-wait-ctrl-{}", Uuid::new_v4());
    let wait_id = create_open_case(&app, &jar, &ws, &wait_title).await;
    let waited = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{wait_id}/decide")),
        Some(&jar),
        Some(json!({ "decision": { "action": "wait", "reason": "control" } })),
    )
    .await;
    assert_eq!(waited.status, StatusCode::OK, "{}", waited.body);
    assert_eq!(
        waited.body["status"], "waiting",
        "a still-open case can still wait via action=wait: {}",
        waited.body
    );
    assert!(
        waited.body["run_id"].is_null(),
        "wait must not invent a run_id: {}",
        waited.body
    );
}

/// POST /decide action=complete on an acting case is allowed (UI Complete
/// while a fleet run is in flight). Launch nulls next_wake_at — plant a
/// leftover due wake so the clear is not a no-op. A second launched case
/// stays acting so this is not a tautology that every case completed.
#[tokio::test]
async fn manual_complete_from_acting_is_allowed_and_clears_wake() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let workspace_id = Uuid::parse_str(&ws).expect("workspace id");

    let one_task = json!({
        "decision": {
            "action": "launch_agents",
            "launch": {
                "title": "one-task",
                "tasks": [{"id": "t1", "title": "T1", "spec": "do the work"}]
            }
        }
    });

    let title = format!("complete-from-acting-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &jar, &ws, &title).await;
    let launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(one_task.clone()),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
    assert_eq!(
        launched.body["status"], "acting",
        "one-task launch_agents must set acting: {}",
        launched.body
    );
    let run_id = launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("launch must return run_id uuid: {}", launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&run_id).is_ok(),
        "run_id must be a uuid: {run_id:?} body={}",
        launched.body
    );
    let decision_count = launched.body["decision_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("decision_count: {}", launched.body));

    let ctrl_title = format!("complete-from-acting-ctrl-{}", Uuid::new_v4());
    let ctrl_id = create_open_case(&app, &jar, &ws, &ctrl_title).await;
    let ctrl_launched = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{ctrl_id}/decide")),
        Some(&jar),
        Some(one_task),
    )
    .await;
    assert_eq!(
        ctrl_launched.status,
        StatusCode::OK,
        "{}",
        ctrl_launched.body
    );
    assert_eq!(
        ctrl_launched.body["status"], "acting",
        "control launch must set acting: {}",
        ctrl_launched.body
    );
    let ctrl_run_id = ctrl_launched.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("control launch run_id: {}", ctrl_launched.body))
        .to_string();
    assert!(
        Uuid::parse_str(&ctrl_run_id).is_ok(),
        "control run_id must be a uuid: {ctrl_run_id:?}"
    );
    assert_ne!(ctrl_id, case_id, "control must be a second case");
    assert_ne!(ctrl_run_id, run_id, "control must have its own fleet run");

    let leftover = chrono::Utc::now() - chrono::Duration::seconds(30);
    let case_uuid = Uuid::parse_str(&case_id).expect("case id");
    let planted = sqlx::query(
        r#"
        UPDATE cases
        SET next_wake_at = $3
        WHERE workspace_id = $1 AND id = $2 AND status = 'acting'
        "#,
    )
    .bind(workspace_id)
    .bind(case_uuid)
    .bind(leftover)
    .execute(&state.db)
    .await
    .expect("plant leftover due wake on acting row");
    assert_eq!(
        planted.rows_affected(),
        1,
        "expected the acting row planted with a leftover wake: {case_id}"
    );

    let got_acting = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_acting.status, StatusCode::OK, "{}", got_acting.body);
    assert_eq!(
        got_acting.body["status"], "acting",
        "planted leftover wake must keep acting: {}",
        got_acting.body
    );
    assert_eq!(
        got_acting.body["run_id"].as_str(),
        Some(run_id.as_str()),
        "plant must keep run_id: {}",
        got_acting.body
    );
    let planted_wake = parse_next_wake_at(&got_acting.body);
    assert!(
        planted_wake <= chrono::Utc::now(),
        "leftover wake must be due so complete is not a no-op clear: {planted_wake} body={}",
        got_acting.body
    );
    assert_eq!(
        got_acting.body["decision_count"].as_i64(),
        Some(decision_count),
        "plant must not bump decision_count: {}",
        got_acting.body
    );

    let completed = call(
        &app,
        "POST",
        &cases_path(&ws, &format!("/{case_id}/decide")),
        Some(&jar),
        Some(json!({
            "decision": { "action": "complete", "note": "completed from UI" }
        })),
    )
    .await;
    assert_eq!(
        completed.status,
        StatusCode::OK,
        "POST /decide complete from acting must be 200, not 409/400: {}",
        completed.body
    );
    assert_ne!(
        completed.status,
        StatusCode::CONFLICT,
        "acting is not terminal: {}",
        completed.body
    );
    assert_ne!(
        completed.status,
        StatusCode::BAD_REQUEST,
        "well-formed complete on acting is not 400: {}",
        completed.body
    );
    assert_eq!(
        completed.body["status"], "completed",
        "manual complete from acting must mark completed: {}",
        completed.body
    );
    assert_ne!(completed.body["status"], "waiting", "{}", completed.body);
    assert_ne!(completed.body["status"], "acting", "{}", completed.body);
    assert_ne!(completed.body["status"], "deciding", "{}", completed.body);
    assert_ne!(completed.body["status"], "open", "{}", completed.body);
    assert!(
        completed.body["next_wake_at"].is_null(),
        "complete must clear leftover next_wake_at, not schedule a new wake: {}",
        completed.body
    );
    let after_count = completed.body["decision_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("decision_count after complete: {}", completed.body));
    assert!(
        after_count > decision_count,
        "complete is a decide; decision_count must bump ({decision_count} → {after_count}): {}",
        completed.body
    );

    let got_done = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{case_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_done.status, StatusCode::OK, "{}", got_done.body);
    assert_eq!(
        got_done.body["status"], "completed",
        "GET after complete must stay completed: {}",
        got_done.body
    );
    assert_ne!(got_done.body["status"], "waiting", "{}", got_done.body);
    assert_ne!(got_done.body["status"], "acting", "{}", got_done.body);
    assert_ne!(got_done.body["status"], "deciding", "{}", got_done.body);
    assert_ne!(got_done.body["status"], "open", "{}", got_done.body);
    assert!(
        got_done.body["next_wake_at"].is_null(),
        "GET must not keep leftover wake or schedule a new one: {}",
        got_done.body
    );
    assert!(
        got_done.body["last_error"].is_null(),
        "complete must leave last_error null: {}",
        got_done.body
    );
    assert_eq!(
        got_done.body["decision_count"].as_i64(),
        Some(after_count),
        "GET decision_count: {}",
        got_done.body
    );
    let events = got_done.body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events: {}", got_done.body));
    let decided_complete: Vec<&Value> = events
        .iter()
        .filter(|e| {
            e["kind"].as_str() == Some("decided")
                && e["payload"]["action"].as_str() == Some("complete")
        })
        .collect();
    assert!(
        !decided_complete.is_empty(),
        "events must include kind=decided action=complete: {}",
        got_done.body
    );
    let completed_ev: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"].as_str() == Some("completed"))
        .collect();
    assert!(
        !completed_ev.is_empty(),
        "events must include kind=completed: {}",
        got_done.body
    );

    let got_ctrl = call(
        &app,
        "GET",
        &cases_path(&ws, &format!("/{ctrl_id}")),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_ctrl.status, StatusCode::OK, "{}", got_ctrl.body);
    assert_eq!(
        got_ctrl.body["status"], "acting",
        "control case was not POSTed complete and must stay acting: {}",
        got_ctrl.body
    );
    assert_eq!(
        got_ctrl.body["run_id"].as_str(),
        Some(ctrl_run_id.as_str()),
        "control run_id must persist: {}",
        got_ctrl.body
    );
    assert_ne!(
        got_ctrl.body["status"], "completed",
        "completing one acting case must not complete the control: {}",
        got_ctrl.body
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
async fn inbox_other_workspace_forbidden() {
    let app = app().await;
    let (_a_email, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let key = format!("other-ws-{}/app#1", Uuid::new_v4());

    let get = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&b_jar), None).await;
    assert_inbox_not_a_member(&get, "B GET A's inbox");

    let monitor = call(
        &app,
        "POST",
        &inbox_path(&a_ws, ""),
        Some(&b_jar),
        Some(json!({
            "enabled": true,
            "providers": ["github"],
            "auto_launch": true,
        })),
    )
    .await;
    assert_inbox_not_a_member(&monitor, "B POST monitor on A's inbox");

    let poll = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/poll"),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_inbox_not_a_member(&poll, "B POST poll on A's inbox");

    let finish = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/finish"),
        Some(&b_jar),
        Some(json!({ "provider": "github", "key": key })),
    )
    .await;
    assert_inbox_not_a_member(&finish, "B POST finish on A's inbox");
}

fn assert_inbox_not_a_member(out: &HttpOut, label: &str) {
    assert_inbox_denied(out);
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{label} must be 403 not-a-member: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out);
    assert!(
        err.to_ascii_lowercase().contains("not a member"),
        "{label} must say not a member of this workspace: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "{label} must not return run_id: {}",
        out.body
    );
}

async fn assert_b_finish_does_not_launch_a(
    app: &axum::Router,
    a_jar: &CookieJar,
    a_inbox: &str,
    out: &HttpOut,
    provider: &str,
    key: &str,
    a_ticket_id: &str,
    a_title: &str,
    snap_state: &Value,
    snap_run: &Value,
    snap_monitor: &Value,
    label: &str,
) {
    assert_foreign_finish_does_not_claim_a(out, a_ticket_id, a_title, label);
    let after = call(app, "GET", a_inbox, Some(a_jar), None).await;
    assert_a_inbox_snapshot(
        &after,
        provider,
        key,
        a_ticket_id,
        a_title,
        snap_state,
        snap_run,
        snap_monitor,
        label,
    );
    if out.status == StatusCode::OK {
        if let Some(run) = out.body.get("run_id").and_then(|v| v.as_str()) {
            assert_ne!(
                inbox_ticket(&after.body, provider, key)["run_id"].as_str(),
                Some(run),
                "{label}: A's ticket must not receive this run_id: {}",
                out.body
            );
        }
    }
}

fn assert_foreign_finish_does_not_claim_a(
    out: &HttpOut,
    a_ticket_id: &str,
    a_title: &str,
    label: &str,
) {
    if out.status == StatusCode::OK {
        for id in [
            out.body.pointer("/ticket/id").and_then(|v| v.as_str()),
            out.body.get("id").and_then(|v| v.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            assert_ne!(
                id, a_ticket_id,
                "{label} 200 must not resolve to A's inbox row: {}",
                out.body
            );
        }
        // finish_and_launch omits ticket.id — A's unique title is the lock that
        // the handler did not treat this as targeting A's row.
        let claimed_title = out.body.pointer("/ticket/title").and_then(|v| v.as_str());
        assert_ne!(
            claimed_title,
            Some(a_title),
            "{label} 200 must not launch/copy A's row (title): {}",
            out.body
        );
        let top_title = out.body.get("title").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !top_title.contains(a_title),
            "{label} 200 must not reuse A's title (would mean finish resolved A's row): {}",
            out.body
        );
        return;
    }
    assert!(
        out.status == StatusCode::FORBIDDEN
            || out.status == StatusCode::NOT_FOUND
            || out.status == StatusCode::BAD_REQUEST,
        "{label} must not succeed against A's row: {} {}",
        out.status,
        out.body
    );
    if out.status == StatusCode::FORBIDDEN {
        let err = http_error_text(out).to_ascii_lowercase();
        assert!(
            err.contains("ticket is not in this workspace") || err.contains("not a member"),
            "{label} 403 must name the deny: {}",
            out.body
        );
    }
}

fn assert_a_inbox_snapshot(
    inbox: &HttpOut,
    provider: &str,
    key: &str,
    ticket_id: &str,
    title: &str,
    state: &Value,
    run_id: &Value,
    monitor: &Value,
    label: &str,
) {
    assert_eq!(
        inbox.status,
        StatusCode::OK,
        "{label}: A's GET must stay 200: {} {}",
        inbox.status,
        inbox.body
    );
    let ticket = inbox_ticket(&inbox.body, provider, key);
    assert_eq!(
        ticket["id"].as_str(),
        Some(ticket_id),
        "{label}: ticket id: {ticket}"
    );
    assert_eq!(
        ticket["title"].as_str(),
        Some(title),
        "{label}: ticket title overwritten: {ticket}"
    );
    assert_eq!(&ticket["state"], state, "{label}: ticket state: {ticket}");
    assert_eq!(
        &ticket["run_id"], run_id,
        "{label}: ticket run_id: {ticket}"
    );
    assert_eq!(
        &inbox.body["monitor"], monitor,
        "{label}: A's monitor must be unchanged: {}",
        inbox.body
    );
}

fn inbox_ticket_ids(body: &Value) -> Vec<String> {
    body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {body}"))
        .iter()
        .filter_map(|t| t["id"].as_str().map(str::to_string))
        .collect()
}

/// B must not list, monitor, poll, or finish A's inbox; knowing A's ticket id
/// on B's routes must not mark_launched or upsert A's monitor.
#[tokio::test]
async fn finish_or_monitor_ticket_in_another_workspace_is_403() {
    use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox, UpsertMonitor};

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let provider = "github";
    let key = format!("cross-ws-{}/app#1", Uuid::new_v4());
    let monitor_marker = format!("a-only-{}", Uuid::new_v4());

    let seeded = ticket_repo::upsert_inbox(
        &state.db,
        &UpsertInbox {
            workspace_id: a_ws_id,
            provider: provider.into(),
            external_key: key.clone(),
            title: "A workspace only".into(),
            url: None,
            summary: "do not finish from B".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("seed A's inbox");
    let a_ticket_id = seeded.id.to_string();
    assert_eq!(seeded.state, "open");
    assert!(seeded.run_id.is_none());

    ticket_repo::upsert_monitor(
        &state.db,
        &UpsertMonitor {
            workspace_id: a_ws_id,
            enabled: true,
            providers: vec!["linear".into()],
            github_repos: vec![monitor_marker.clone()],
            jira_jql: Some("assignee = a-only".into()),
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: false,
            started_by: None,
        },
    )
    .await
    .expect("seed A's monitor");

    let before = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "A's GET before B: {}",
        before.body
    );
    let snap_ticket = inbox_ticket(&before.body, provider, &key);
    let snap_state = snap_ticket["state"].clone();
    let snap_run = snap_ticket["run_id"].clone();
    let snap_monitor = before.body["monitor"].clone();
    let a_title = "A workspace only";
    assert_eq!(snap_ticket["id"].as_str(), Some(a_ticket_id.as_str()));
    assert_eq!(snap_ticket["title"].as_str(), Some(a_title));
    assert_eq!(snap_state.as_str(), Some("open"));
    assert!(snap_run.is_null());
    assert_eq!(
        snap_monitor["providers"],
        json!(["linear"]),
        "seeded monitor: {}",
        before.body
    );
    assert!(
        inbox_monitor_providers(&before.body)
            .iter()
            .any(|p| p == "linear"),
        "A's GET must show the seeded monitor so this is not a tautology: {}",
        before.body
    );

    let a_inbox = inbox_path(&a_ws, "");
    let a_finish = inbox_path(&a_ws, "/finish");
    let finish_body = json!({
        "provider": provider,
        "key": key,
        "ticket": a_ticket_id,
    });

    let get_as_b = call(&app, "GET", &a_inbox, Some(&b_jar), None).await;
    assert_inbox_not_a_member(&get_as_b, "B GET A's inbox");

    let monitor_as_b = call(
        &app,
        "POST",
        &a_inbox,
        Some(&b_jar),
        Some(json!({
            "enabled": true,
            "providers": ["github"],
            "auto_launch": true,
        })),
    )
    .await;
    assert_inbox_not_a_member(&monitor_as_b, "B POST monitor on A's inbox");

    let poll_as_b = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/poll"),
        Some(&b_jar),
        Some(json!({})),
    )
    .await;
    assert_inbox_not_a_member(&poll_as_b, "B POST poll on A's inbox");

    let finish_as_b = call(
        &app,
        "POST",
        &a_finish,
        Some(&b_jar),
        Some(finish_body.clone()),
    )
    .await;
    assert_inbox_not_a_member(&finish_as_b, "B POST finish on A's inbox");

    let finish_ticket_only = call(
        &app,
        "POST",
        &a_finish,
        Some(&b_jar),
        Some(json!({ "ticket": a_ticket_id })),
    )
    .await;
    assert_inbox_not_a_member(&finish_ticket_only, "B POST finish ticket=id on A's inbox");

    let after_a_routes = call(&app, "GET", &a_inbox, Some(&a_jar), None).await;
    assert_a_inbox_snapshot(
        &after_a_routes,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "after B on A's routes",
    );

    let b_finish = inbox_path(&b_ws, "/finish");
    let via_ticket = call(
        &app,
        "POST",
        &b_finish,
        Some(&b_jar),
        Some(json!({ "ticket": a_ticket_id })),
    )
    .await;
    assert_b_finish_does_not_launch_a(
        &app,
        &a_jar,
        &a_inbox,
        &via_ticket,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "B finish ticket=A id",
    )
    .await;

    let via_key = call(
        &app,
        "POST",
        &b_finish,
        Some(&b_jar),
        Some(json!({ "key": a_ticket_id })),
    )
    .await;
    assert_b_finish_does_not_launch_a(
        &app,
        &a_jar,
        &a_inbox,
        &via_key,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "B finish key=A id",
    )
    .await;

    let via_provider_key = call(
        &app,
        "POST",
        &b_finish,
        Some(&b_jar),
        Some(json!({ "provider": provider, "key": key })),
    )
    .await;
    assert_b_finish_does_not_launch_a(
        &app,
        &a_jar,
        &a_inbox,
        &via_provider_key,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "B finish A's provider+key",
    )
    .await;

    let via_ticket_and_key = call(
        &app,
        "POST",
        &b_finish,
        Some(&b_jar),
        Some(finish_body.clone()),
    )
    .await;
    assert_b_finish_does_not_launch_a(
        &app,
        &a_jar,
        &a_inbox,
        &via_ticket_and_key,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "B finish ticket+provider+key of A",
    )
    .await;

    let b_monitor = call(
        &app,
        "POST",
        &inbox_path(&b_ws, ""),
        Some(&b_jar),
        Some(json!({
            "enabled": true,
            "providers": ["github"],
            "auto_launch": true,
        })),
    )
    .await;
    assert!(
        b_monitor.status == StatusCode::BAD_REQUEST || b_monitor.status == StatusCode::OK,
        "B monitor on B may be 400 no-source or 200 for B only: {} {}",
        b_monitor.status,
        b_monitor.body
    );
    if b_monitor.status == StatusCode::OK {
        assert_ne!(
            b_monitor.body.get("providers"),
            Some(&json!(["linear"])),
            "B's 200 monitor must not be A's row: {}",
            b_monitor.body
        );
        let copied_marker = b_monitor.body["github_repos"]
            .as_array()
            .map(|a| {
                a.iter()
                    .any(|v| v.as_str() == Some(monitor_marker.as_str()))
            })
            .unwrap_or(false);
        assert!(
            !copied_marker,
            "B's monitor must not copy A's github_repos marker: {}",
            b_monitor.body
        );
    }

    let after_b_monitor = call(&app, "GET", &a_inbox, Some(&a_jar), None).await;
    assert_a_inbox_snapshot(
        &after_b_monitor,
        provider,
        &key,
        &a_ticket_id,
        a_title,
        &snap_state,
        &snap_run,
        &snap_monitor,
        "after B monitor on B's routes",
    );
    assert!(
        inbox_monitor_providers(&after_b_monitor.body)
            .iter()
            .any(|p| p == "linear"),
        "A's monitor must still list linear: {}",
        after_b_monitor.body
    );
    assert!(
        !inbox_monitor_is_enabled_auto_launch(&after_b_monitor.body),
        "B must not flip A's monitor to auto_launch: {}",
        after_b_monitor.body
    );

    let b_inbox = call(&app, "GET", &inbox_path(&b_ws, ""), Some(&b_jar), None).await;
    assert_eq!(
        b_inbox.status,
        StatusCode::OK,
        "B GET own inbox: {}",
        b_inbox.body
    );
    assert!(
        !inbox_ticket_ids(&b_inbox.body)
            .iter()
            .any(|id| id == &a_ticket_id),
        "B's inbox must not list A's ticket id {a_ticket_id}: {}",
        b_inbox.body
    );
    let b_repos = b_inbox.body["monitor"]["github_repos"]
        .as_array()
        .map(|a| {
            a.iter()
                .any(|v| v.as_str() == Some(monitor_marker.as_str()))
        })
        .unwrap_or(false);
    assert!(
        !b_repos,
        "B's monitor must not be A's (github_repos marker): {}",
        b_inbox.body
    );
}

fn http_error_text(out: &HttpOut) -> String {
    let err = out.body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    format!("{err} {}", out.raw)
}

fn mentions_unknown_provider(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("unknown") && lower.contains("provider")
}

fn assert_unknown_provider_http(out: &HttpOut, label: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{label} must be 400 (not 404/200): {} {}",
        out.status,
        out.body
    );
    assert!(
        mentions_unknown_provider(&http_error_text(out)),
        "{label} body must mention unknown+provider: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "{label} must not return run_id: {}",
        out.body
    );
}

fn find_inbox_ticket<'a>(body: &'a Value, provider: &str, key: &str) -> Option<&'a Value> {
    body["tickets"]
        .as_array()?
        .iter()
        .find(|t| t["provider"] == provider && t["key"] == key)
}

fn inbox_monitor_providers(body: &Value) -> Vec<String> {
    body["monitor"]["providers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn inbox_monitor_is_enabled_auto_launch(body: &Value) -> bool {
    let mon = &body["monitor"];
    !mon.is_null()
        && mon.get("enabled").and_then(|v| v.as_bool()) == Some(true)
        && mon.get("auto_launch").and_then(|v| v.as_bool()) == Some(true)
}

fn assert_inbox_has_no_typo_or_auto_launch(body: &Value, typo: &str, label: &str) {
    assert!(
        !inbox_monitor_is_enabled_auto_launch(body),
        "{label}: monitor must be null or not enabled with auto_launch: {body}"
    );
    assert!(
        !inbox_monitor_providers(body).iter().any(|p| p == typo),
        "{label}: monitor must not list {typo}: {body}"
    );
}

fn inbox_monitor_auto_launch(body: &Value) -> Option<bool> {
    let mon = &body["monitor"];
    if mon.is_null() {
        return None;
    }
    mon.get("auto_launch").and_then(|v| v.as_bool())
}

fn assert_inbox_auto_launch_not_armed(body: &Value, label: &str) {
    assert!(
        !inbox_monitor_is_enabled_auto_launch(body),
        "{label}: monitor must be null or not enabled with auto_launch: {body}"
    );
    assert_ne!(
        inbox_monitor_auto_launch(body),
        Some(true),
        "{label}: auto_launch must not be armed with no listen source: {body}"
    );
}

fn assert_no_listen_source_http(out: &HttpOut, label: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{label} must be 400 (not 404/200): {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out).to_ascii_lowercase();
    assert!(
        err.contains("listen") || err.contains("connected"),
        "{label} body must name the missing listen source: {}",
        out.body
    );
    assert!(
        !mentions_unknown_provider(&err),
        "{label} is empty, not unknown provider: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "{label} must not return run_id: {}",
        out.body
    );
    assert!(
        out.body.get("auto_launch").is_none()
            || out.body["auto_launch"].is_null()
            || out.body["auto_launch"] == json!(false),
        "{label} 400 must not echo auto_launch true: {}",
        out.body
    );
}

/// Unknown inbox provider is 400 on Finish (not 404/200). Missing fields stay
/// 400/404 as today. A known TASK_PROVIDERS name must not 400 as unknown.
#[tokio::test]
async fn inbox_finish_missing_or_unknown_provider_rejected() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let finish_path = inbox_path(&ws, "/finish");

    let missing = call(&app, "POST", &finish_path, Some(&jar), Some(json!({}))).await;
    assert!(
        missing.status == StatusCode::BAD_REQUEST || missing.status == StatusCode::NOT_FOUND,
        "missing fields must be 400/404, not launch: {} {}",
        missing.status,
        missing.body
    );
    assert_ne!(
        missing.body.get("ok"),
        Some(&json!(true)),
        "missing fields must not return ok: {}",
        missing.body
    );
    assert!(
        missing.body.get("run_id").is_none() || missing.body["run_id"].is_null(),
        "missing fields must not return run_id: {}",
        missing.body
    );

    let key = format!("unk-finish-{}", Uuid::new_v4());
    let unknown = call(
        &app,
        "POST",
        &finish_path,
        Some(&jar),
        Some(json!({ "provider": "not_a_real_provider", "key": key })),
    )
    .await;
    assert_unknown_provider_http(&unknown, "unknown finish provider");

    let inbox = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(inbox.status, StatusCode::OK, "GET inbox: {}", inbox.body);
    assert!(
        find_inbox_ticket(&inbox.body, "not_a_real_provider", &key).is_none(),
        "GET inbox must not seed unknown provider+key: {}",
        inbox.body
    );
    let tickets = inbox.body["tickets"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        tickets
            .iter()
            .all(|t| t.get("run_id").is_none() || t["run_id"].is_null()),
        "GET inbox must not attach a run after unknown finish: {}",
        inbox.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "GET runs: {}", runs.body);
    let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
    assert!(
        run_list.is_empty(),
        "unknown finish must not insert a run: {}",
        runs.body
    );

    let known_key = format!("known-finish-{}/app#1", Uuid::new_v4());
    let known = call(
        &app,
        "POST",
        &finish_path,
        Some(&jar),
        Some(json!({ "provider": "github", "key": known_key })),
    )
    .await;
    if known.status == StatusCode::BAD_REQUEST {
        assert!(
            !mentions_unknown_provider(&http_error_text(&known)),
            "known TASK_PROVIDERS name must not 400 as unknown provider: {}",
            known.body
        );
    } else {
        assert_ne!(
            known.status,
            StatusCode::NOT_FOUND,
            "github finish must not 404 as unknown provider: {}",
            known.body
        );
    }
}

/// HTTP monitor + chat `monitor_tickets` / `finish_ticket` reject names not
/// in TASK_PROVIDERS. Silent filter or upsert-then-400 must not write.
#[tokio::test]
async fn inbox_and_chat_unknown_provider_rejected() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let typo = "not_a_real_provider";
    let inbox = inbox_path(&ws, "");

    let typo_only = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": [typo],
            "auto_launch": true,
        })),
    )
    .await;
    assert_unknown_provider_http(&typo_only, "monitor unknown-only");

    let after_typo = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_typo.status,
        StatusCode::OK,
        "GET after typo monitor: {}",
        after_typo.body
    );
    assert_inbox_has_no_typo_or_auto_launch(&after_typo.body, typo, "after typo-only monitor");
    assert!(
        after_typo.body["monitor"].is_null()
            || after_typo.body.get("monitor").is_none()
            || inbox_monitor_providers(&after_typo.body).is_empty(),
        "typo-only monitor must not upsert a row: {}",
        after_typo.body
    );

    let mixed = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": ["github", typo],
            "auto_launch": true,
        })),
    )
    .await;
    assert_unknown_provider_http(&mixed, "monitor mixed github+typo");

    let after_mixed = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_mixed.status,
        StatusCode::OK,
        "GET after mixed: {}",
        after_mixed.body
    );
    assert_inbox_has_no_typo_or_auto_launch(&after_mixed.body, typo, "after mixed monitor");
    let mixed_providers = inbox_monitor_providers(&after_mixed.body);
    assert!(
        after_mixed.body["monitor"].is_null()
            || after_mixed.body.get("monitor").is_none()
            || mixed_providers.is_empty(),
        "mixed list must not silently upsert a github-only monitor: {}",
        after_mixed.body
    );
    assert!(
        !mixed_providers.iter().any(|p| p == "github"),
        "must not keep github and drop the typo: {}",
        after_mixed.body
    );

    let seed = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": false,
            "providers": ["github"],
            "auto_launch": false,
        })),
    )
    .await;
    assert_eq!(seed.status, StatusCode::OK, "seed monitor: {}", seed.body);
    assert!(
        seed.body.get("poll").is_none() || seed.body["poll"].is_null(),
        "disabled seed must not poll: {}",
        seed.body
    );

    let after_unknown = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": [typo],
            "auto_launch": true,
        })),
    )
    .await;
    assert_unknown_provider_http(&after_unknown, "monitor unknown after seed");

    let after_seed = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_seed.status,
        StatusCode::OK,
        "GET after unknown-on-seed: {}",
        after_seed.body
    );
    assert_eq!(
        after_seed.body["monitor"]["enabled"],
        json!(false),
        "seeded monitor must stay disabled: {}",
        after_seed.body
    );
    assert_eq!(
        after_seed.body["monitor"]["auto_launch"],
        json!(false),
        "seeded monitor must stay auto_launch false: {}",
        after_seed.body
    );
    assert_eq!(
        inbox_monitor_providers(&after_seed.body),
        vec!["github".to_string()],
        "seeded providers must stay github: {}",
        after_seed.body
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let (mon_content, mon_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "monitor_tickets",
        &json!({
            "enabled": true,
            "providers": [typo],
            "auto_launch": true,
        }),
        &llm,
    )
    .await;
    assert!(
        !mon_action.ok,
        "chat monitor_tickets unknown must fail: action={mon_action:?} content={mon_content}"
    );
    let mon_err = format!(
        "{} {}",
        mon_action.summary,
        mon_content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    );
    assert!(
        mentions_unknown_provider(&mon_err),
        "chat monitor_tickets must mention unknown+provider: action={mon_action:?} content={mon_content}"
    );
    assert!(
        mon_action.run_id.is_none(),
        "chat monitor_tickets must not return a run: {mon_action:?}"
    );

    let after_chat_mon = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_chat_mon.status,
        StatusCode::OK,
        "GET after chat monitor: {}",
        after_chat_mon.body
    );
    assert_inbox_has_no_typo_or_auto_launch(&after_chat_mon.body, typo, "after chat monitor");
    assert_eq!(
        after_chat_mon.body["monitor"]["enabled"],
        json!(false),
        "chat monitor must not enable the seeded row: {}",
        after_chat_mon.body
    );
    assert_eq!(
        after_chat_mon.body["monitor"]["auto_launch"],
        json!(false),
        "chat monitor must not set auto_launch: {}",
        after_chat_mon.body
    );
    assert_eq!(
        inbox_monitor_providers(&after_chat_mon.body),
        vec!["github".to_string()],
        "chat monitor must not add the typo: {}",
        after_chat_mon.body
    );

    let finish_key = format!("unk-chat-finish-{}", Uuid::new_v4());
    let (fin_content, fin_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "finish_ticket",
        &json!({ "provider": typo, "key": finish_key }),
        &llm,
    )
    .await;
    assert!(
        !fin_action.ok,
        "chat finish_ticket unknown must fail: action={fin_action:?} content={fin_content}"
    );
    let fin_err = format!(
        "{} {}",
        fin_action.summary,
        fin_content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    );
    assert!(
        mentions_unknown_provider(&fin_err),
        "chat finish_ticket must mention unknown+provider: action={fin_action:?} content={fin_content}"
    );
    assert!(
        fin_action.run_id.is_none(),
        "chat finish_ticket must not return a run: {fin_action:?}"
    );
    assert!(
        fin_content.get("run_id").is_none() || fin_content["run_id"].is_null(),
        "chat finish_ticket must not put run_id in content: {fin_content}"
    );

    let after_chat_fin = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_chat_fin.status,
        StatusCode::OK,
        "GET after chat finish: {}",
        after_chat_fin.body
    );
    assert!(
        find_inbox_ticket(&after_chat_fin.body, typo, &finish_key).is_none(),
        "chat finish_ticket must not seed inbox: {}",
        after_chat_fin.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "GET runs: {}", runs.body);
    let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
    assert!(
        run_list.is_empty(),
        "chat finish_ticket must not insert a run: {}",
        runs.body
    );
}

/// Empty / omitted / blank monitor provider lists + auto_launch, with no
/// connected integration, must 400 and must not arm auto_launch. Invalid
/// names stay 400 (sibling list-inbox-unknown-provider-http owns the
/// full typo matrix). Explicit disabled github seed is the control.
#[tokio::test]
async fn inbox_empty_or_invalid_monitor_provider_list_fails_closed() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let inbox = inbox_path(&ws, "");

    let snap = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "GET snapshot: {}", snap.body);
    assert_inbox_auto_launch_not_armed(&snap.body, "fresh inbox");
    assert!(
        snap.body["monitor"].is_null()
            || snap.body.get("monitor").is_none()
            || inbox_monitor_providers(&snap.body).is_empty(),
        "fresh signup must not already have a listen list: {}",
        snap.body
    );

    for (label, body) in [
        (
            "empty providers + auto_launch",
            json!({
                "enabled": true,
                "providers": [],
                "auto_launch": true,
            }),
        ),
        (
            "omitted providers + auto_launch (UI Start monitor)",
            json!({
                "enabled": true,
                "auto_launch": true,
            }),
        ),
        (
            "blank-only providers + auto_launch",
            json!({
                "enabled": true,
                "providers": ["", "  ", "\t"],
                "auto_launch": true,
            }),
        ),
    ] {
        let out = call(&app, "POST", &inbox, Some(&jar), Some(body)).await;
        assert_no_listen_source_http(&out, label);

        let after = call(&app, "GET", &inbox, Some(&jar), None).await;
        assert_eq!(
            after.status,
            StatusCode::OK,
            "GET after {label}: {}",
            after.body
        );
        assert_inbox_auto_launch_not_armed(&after.body, &format!("after {label}"));
        assert!(
            after.body["monitor"].is_null()
                || after.body.get("monitor").is_none()
                || inbox_monitor_providers(&after.body).is_empty(),
            "{label} must not upsert a listen list: {}",
            after.body
        );
    }

    let typo = "not_a_real_provider";
    let invalid = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": [typo],
            "auto_launch": true,
        })),
    )
    .await;
    assert_unknown_provider_http(&invalid, "invalid provider + auto_launch");
    let after_invalid = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_invalid.status,
        StatusCode::OK,
        "GET after invalid: {}",
        after_invalid.body
    );
    assert_inbox_has_no_typo_or_auto_launch(&after_invalid.body, typo, "after invalid monitor");
    assert_inbox_auto_launch_not_armed(&after_invalid.body, "after invalid monitor");

    let arm_disabled = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": false,
            "providers": [],
            "auto_launch": true,
        })),
    )
    .await;
    let after_arm = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_arm.status,
        StatusCode::OK,
        "GET after disabled empty auto_launch: {}",
        after_arm.body
    );
    assert_inbox_auto_launch_not_armed(&after_arm.body, "disabled empty + auto_launch");
    if arm_disabled.status == StatusCode::OK {
        assert_ne!(
            arm_disabled.body.get("auto_launch"),
            Some(&json!(true)),
            "disabled empty list must not persist auto_launch: {}",
            arm_disabled.body
        );
    } else {
        assert_eq!(
            arm_disabled.status,
            StatusCode::BAD_REQUEST,
            "disabled empty + auto_launch must 400 or not persist auto_launch: {} {}",
            arm_disabled.status,
            arm_disabled.body
        );
    }

    let seed = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": false,
            "providers": ["github"],
            "auto_launch": false,
        })),
    )
    .await;
    assert_eq!(seed.status, StatusCode::OK, "seed monitor: {}", seed.body);
    assert!(
        seed.body.get("poll").is_none() || seed.body["poll"].is_null(),
        "disabled seed must not poll: {}",
        seed.body
    );

    let after_empty = call(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": [],
            "auto_launch": true,
        })),
    )
    .await;
    assert_no_listen_source_http(&after_empty, "empty + auto_launch after seed");

    let after_seed = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_seed.status,
        StatusCode::OK,
        "GET after empty-on-seed: {}",
        after_seed.body
    );
    assert_eq!(
        after_seed.body["monitor"]["enabled"],
        json!(false),
        "seeded monitor must stay disabled: {}",
        after_seed.body
    );
    assert_eq!(
        after_seed.body["monitor"]["auto_launch"],
        json!(false),
        "seeded monitor must stay auto_launch false: {}",
        after_seed.body
    );
    assert_eq!(
        inbox_monitor_providers(&after_seed.body),
        vec!["github".to_string()],
        "seeded providers must stay github: {}",
        after_seed.body
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");
    let workspace_id = Uuid::parse_str(&ws).expect("workspace uuid");
    let llm = fleet_server::chat_tools::ChatLlmContext {
        runner_id: None,
        effort: None,
        provider: "grok".into(),
        model: "grok-4.5".into(),
    };

    let (mon_content, mon_action) = fleet_server::chat_tools::execute_tool(
        &state,
        workspace_id,
        user_id,
        "monitor_tickets",
        &json!({
            "enabled": true,
            "providers": [],
            "auto_launch": true,
        }),
        &llm,
    )
    .await;
    assert!(
        !mon_action.ok,
        "chat monitor_tickets empty+auto_launch must fail: action={mon_action:?} content={mon_content}"
    );
    let mon_err = format!(
        "{} {}",
        mon_action.summary,
        mon_content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    )
    .to_ascii_lowercase();
    assert!(
        mon_err.contains("listen") || mon_err.contains("connected"),
        "chat monitor_tickets empty must name the missing source: action={mon_action:?} content={mon_content}"
    );
    assert!(
        !mentions_unknown_provider(&mon_err),
        "chat empty list is not unknown provider: action={mon_action:?} content={mon_content}"
    );
    assert!(
        mon_action.run_id.is_none(),
        "chat monitor_tickets must not return a run: {mon_action:?}"
    );

    let after_chat = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after_chat.status,
        StatusCode::OK,
        "GET after chat empty monitor: {}",
        after_chat.body
    );
    assert_eq!(
        after_chat.body["monitor"]["enabled"],
        json!(false),
        "chat empty must not enable the seeded row: {}",
        after_chat.body
    );
    assert_eq!(
        after_chat.body["monitor"]["auto_launch"],
        json!(false),
        "chat empty must not set auto_launch: {}",
        after_chat.body
    );
    assert_eq!(
        inbox_monitor_providers(&after_chat.body),
        vec!["github".to_string()],
        "chat empty must not clear the seed: {}",
        after_chat.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(runs.status, StatusCode::OK, "GET runs: {}", runs.body);
    let run_list = runs.body["runs"].as_array().cloned().unwrap_or_default();
    assert!(
        run_list.is_empty(),
        "empty/invalid monitor must not insert a run: {}",
        runs.body
    );
}

fn inbox_ticket<'a>(body: &'a Value, provider: &str, key: &str) -> &'a Value {
    body["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("tickets: {body}"))
        .iter()
        .find(|t| t["provider"] == provider && t["key"] == key)
        .unwrap_or_else(|| panic!("missing {provider}:{key}: {body}"))
}

fn run_ids_matching<'a>(body: &'a Value, run_id: &str) -> Vec<&'a Value> {
    body["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("runs: {body}"))
        .iter()
        .filter(|r| r["id"].as_str() == Some(run_id))
        .collect()
}

/// Second Finish of the same inbox key reuses the first run and must not
/// insert another accepted plan or overwrite mark_ticket_launched.
#[tokio::test]
async fn inbox_finish_already_launched_does_not_insert_a_second_run() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let key = format!("already-launched-{}/app#1", Uuid::new_v4());
    let other_key = format!("already-launched-{}/app#1", Uuid::new_v4());
    let finish_path = inbox_path(&ws, "/finish");
    let body_same = json!({ "provider": "github", "key": key });
    let body_other = json!({ "provider": "github", "key": other_key });

    let first = call(
        &app,
        "POST",
        &finish_path,
        Some(&jar),
        Some(body_same.clone()),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "first finish: {}", first.body);
    assert_eq!(
        first.body.get("ok"),
        Some(&json!(true)),
        "first finish ok: {}",
        first.body
    );
    let first_run = first.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("first finish run_id: {}", first.body))
        .to_string();
    assert!(
        Uuid::parse_str(&first_run).is_ok(),
        "first finish run_id must be a uuid: {first_run}"
    );

    let inbox = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        inbox.status,
        StatusCode::OK,
        "inbox after first: {}",
        inbox.body
    );
    let ticket = inbox_ticket(&inbox.body, "github", &key);
    assert_eq!(
        ticket["state"].as_str(),
        Some("launched"),
        "first finish must mark launched: {ticket}"
    );
    assert_eq!(
        ticket["run_id"].as_str(),
        Some(first_run.as_str()),
        "inbox run_id must match first finish: {ticket}"
    );

    let second = call(
        &app,
        "POST",
        &finish_path,
        Some(&jar),
        Some(body_same.clone()),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "second finish: {}",
        second.body
    );
    assert_eq!(
        second.body.get("ok"),
        Some(&json!(true)),
        "second finish ok: {}",
        second.body
    );
    let second_run = second.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("second finish run_id: {}", second.body));
    assert_eq!(
        second_run, first_run,
        "second finish must reuse the first run_id, not invent a new uuid: {}",
        second.body
    );

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        runs.status,
        StatusCode::OK,
        "runs after second: {}",
        runs.body
    );
    let matching = run_ids_matching(&runs.body, &first_run);
    assert_eq!(
        matching.len(),
        1,
        "exactly one run for the first id: {}",
        runs.body
    );
    let run_list = runs.body["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("runs: {}", runs.body));
    assert_eq!(
        run_list.len(),
        1,
        "second finish must not insert another accepted plan: {}",
        runs.body
    );

    let inbox_again = call(&app, "GET", &inbox_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        inbox_again.status,
        StatusCode::OK,
        "inbox after second: {}",
        inbox_again.body
    );
    let ticket_again = inbox_ticket(&inbox_again.body, "github", &key);
    assert_eq!(
        ticket_again["state"].as_str(),
        Some("launched"),
        "ticket must stay launched: {ticket_again}"
    );
    assert_eq!(
        ticket_again["run_id"].as_str(),
        Some(first_run.as_str()),
        "second finish must not overwrite mark_ticket_launched: {ticket_again}"
    );

    let other = call(&app, "POST", &finish_path, Some(&jar), Some(body_other)).await;
    assert_eq!(
        other.status,
        StatusCode::OK,
        "other key finish: {}",
        other.body
    );
    assert_eq!(
        other.body.get("ok"),
        Some(&json!(true)),
        "other key ok: {}",
        other.body
    );
    let other_run = other.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("other key run_id: {}", other.body));
    assert_ne!(
        other_run, first_run,
        "a different key's first Finish must insert a distinct run_id: {}",
        other.body
    );
    assert!(
        Uuid::parse_str(other_run).is_ok(),
        "other key run_id must be a uuid: {other_run}"
    );

    let runs_after = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        runs_after.status,
        StatusCode::OK,
        "runs after other key: {}",
        runs_after.body
    );
    assert_eq!(
        run_ids_matching(&runs_after.body, &first_run).len(),
        1,
        "first run must still appear once: {}",
        runs_after.body
    );
    assert_eq!(
        run_ids_matching(&runs_after.body, other_run).len(),
        1,
        "other key must appear as its own run: {}",
        runs_after.body
    );
}

fn assert_inbox_poll_skipped_no_monitor(out: &HttpOut, label: &str) {
    assert_eq!(
        out.status,
        StatusCode::OK,
        "{label} must be 200 skipped no monitor, not {}: {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.body.get("skipped"),
        Some(&json!(true)),
        "{label} must set skipped: true: {}",
        out.body
    );
    let reason = out.body["reason"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        reason.contains("no monitor"),
        "{label} reason must contain no monitor: {}",
        out.body
    );
}

#[tokio::test]
async fn inbox_poll_without_monitor_is_not_500() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let inbox = inbox_path(&ws, "");

    let snap = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "GET /inbox before poll: {}",
        snap.body
    );
    let snap_tickets = inbox_ticket_snapshot(&snap.body);

    let out = call(
        &app,
        "POST",
        &inbox_path(&ws, "/poll"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_inbox_poll_skipped_no_monitor(&out, "poll with no monitor");

    let after = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /inbox after poll: {}",
        after.body
    );
    assert_eq!(
        inbox_ticket_snapshot(&after.body),
        snap_tickets,
        "GET /inbox tickets must stay the snapshot after no-monitor poll: before={} after={}",
        snap.body,
        after.body
    );
}

fn inbox_ticket_snapshot(body: &Value) -> Vec<(String, String, String)> {
    body["tickets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| {
            (
                t["provider"].as_str().unwrap_or("").to_string(),
                t["key"].as_str().unwrap_or("").to_string(),
                t["id"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn inbox_github_issue_keys(body: &Value) -> Vec<String> {
    body["tickets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            let key = t["key"].as_str().unwrap_or("");
            fleet_server::tickets::parse_github_owner_repo_num(key).map(|_| key.to_string())
        })
        .collect()
}

fn inbox_fleet_inbox_source_runs(body: &Value) -> Vec<Value> {
    body["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["source"].as_str() == Some("inbox"))
        .collect()
}

fn inbox_disconnected_hay(out: &HttpOut) -> String {
    format!("{} {}", out.raw, out.body)
}

fn assert_inbox_raw_omits_access_token(out: &HttpOut, label: &str) {
    let hay = inbox_disconnected_hay(out);
    assert!(
        !hay.contains("access_token"),
        "{label} raw body must not contain access_token: {}",
        out.raw
    );
}

fn assert_inbox_did_not_open_github(out: &HttpOut, label: &str) {
    let hay = inbox_disconnected_hay(out).to_ascii_lowercase();
    for needle in ["api.github.com", "github.com"] {
        assert!(
            !hay.contains(needle),
            "{label} must not open {needle} (no live Connect — a connect attempt is a fail): {}",
            out.raw
        );
    }
    let transport = hay.contains("cannot reach")
        || hay.contains("error sending request")
        || hay.contains("error trying to connect")
        || hay.contains("timed out")
        || hay.contains("timeout")
        || hay.contains("dns")
        || hay.contains("requires authentication")
        || hay.contains("bad credentials")
        || hay.contains("rate limit")
        || hay.contains("documentation_url")
        || hay.contains("html_url");
    assert!(
        !transport,
        "{label} must fail closed before an anonymous GitHub GET (transport / public-API error is a fail): {}",
        out.raw
    );
}

fn inbox_poll_says_skipped_disconnected_or_disabled(out: &HttpOut) -> bool {
    let hay = inbox_disconnected_hay(out).to_ascii_lowercase();
    hay.contains("skipped")
        || hay.contains("not_connected")
        || hay.contains("not connected")
        || hay.contains("disabled")
}

async fn call_inbox_disconnected_timed(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    body: Option<Value>,
    label: &str,
) -> HttpOut {
    tokio::time::timeout(
        Duration::from_secs(5),
        call(app, method, uri, cookies, body),
    )
    .await
    .unwrap_or_else(|_| panic!("{label} hung >5s — must not open api.github.com / github.com"))
}

/// Monitor/poll GitHub with no Connect must not hit GitHub, insert
/// owner/repo#n tickets, or auto_launch an inbox-source run.
/// `providers:[github]` is a known name (not the empty-list 400); poll
/// owns `not_connected` via `proxy::invoke` (no anonymous GET).
#[tokio::test]
async fn inbox_poll_disconnected_github_does_not_hit_upstream_or_insert_tickets() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let inbox = inbox_path(&ws, "");
    let poll_path = inbox_path(&ws, "/poll");

    // Control: same contract as inbox_poll_without_monitor_is_not_500.
    let no_monitor = call(&app, "POST", &poll_path, Some(&jar), Some(json!({}))).await;
    assert_inbox_poll_skipped_no_monitor(&no_monitor, "poll with no monitor");
    assert_inbox_raw_omits_access_token(&no_monitor, "poll with no monitor");
    assert_inbox_did_not_open_github(&no_monitor, "poll with no monitor");

    let snap = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(snap.status, StatusCode::OK, "GET snapshot: {}", snap.body);
    assert!(
        snap.body["monitor"].is_null()
            || snap.body.get("monitor").is_none()
            || snap.body["monitor"]["enabled"] == json!(false),
        "fresh inbox monitor must be null or disabled: {}",
        snap.body
    );
    let snap_tickets = inbox_ticket_snapshot(&snap.body);
    assert!(
        snap_tickets.is_empty(),
        "fresh inbox must have no tickets: {}",
        snap.body
    );
    assert!(
        inbox_github_issue_keys(&snap.body).is_empty(),
        "fresh inbox must have no owner/repo#n row: {}",
        snap.body
    );

    let snap_runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        snap_runs.status,
        StatusCode::OK,
        "GET runs snapshot: {}",
        snap_runs.body
    );
    let snap_inbox_runs = inbox_fleet_inbox_source_runs(&snap_runs.body);
    assert!(
        snap_inbox_runs.is_empty(),
        "fresh workspace must have no inbox-source run: {}",
        snap_runs.body
    );

    let connected = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/integrations"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        connected.status,
        StatusCode::OK,
        "GET integrations: {}",
        connected.body
    );
    let github_row = connected.body["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == "github" || i["provider"] == "github")
        .unwrap_or_else(|| panic!("github catalog row: {}", connected.body));
    assert_ne!(
        github_row.get("connected"),
        Some(&json!(true)),
        "this lock requires no live Connect: {}",
        connected.body
    );

    let (_b_email, b_jar, _b_ws) = signup_and_login(&app).await;
    let other_poll = call(&app, "POST", &poll_path, Some(&b_jar), Some(json!({}))).await;
    assert_inbox_not_a_member(&other_poll, "B POST poll on A's inbox");
    assert_inbox_raw_omits_access_token(&other_poll, "other-ws POST /poll");
    assert_inbox_did_not_open_github(&other_poll, "other-ws POST /poll");

    let anon_poll = call(&app, "POST", &poll_path, None, Some(json!({}))).await;
    assert_eq!(
        anon_poll.status,
        StatusCode::UNAUTHORIZED,
        "unauth POST /poll must stay 401: {} {}",
        anon_poll.status,
        anon_poll.body
    );
    assert!(
        http_error_text(&anon_poll)
            .to_ascii_lowercase()
            .contains("not authenticated"),
        "unauth POST /poll must say not authenticated: {}",
        anon_poll.body
    );
    assert_inbox_raw_omits_access_token(&anon_poll, "unauth POST /poll");
    assert_inbox_did_not_open_github(&anon_poll, "unauth POST /poll");

    let monitor = call_inbox_disconnected_timed(
        &app,
        "POST",
        &inbox,
        Some(&jar),
        Some(json!({
            "enabled": true,
            "providers": ["github"],
            "auto_launch": true,
        })),
        "POST /inbox github + auto_launch with no Connect",
    )
    .await;
    assert!(
        !monitor.status.is_server_error(),
        "POST /inbox with github + auto_launch and no Connect must not 5xx: {} {}",
        monitor.status,
        monitor.body
    );
    assert_inbox_raw_omits_access_token(&monitor, "POST /inbox disconnected github");
    assert_inbox_did_not_open_github(&monitor, "POST /inbox disconnected github");
    assert!(
        monitor.body.get("run_id").is_none() || monitor.body["run_id"].is_null(),
        "POST /inbox must not include a run_id: {}",
        monitor.body
    );
    if monitor.status == StatusCode::BAD_REQUEST {
        let err = http_error_text(&monitor).to_ascii_lowercase();
        assert!(
            err.contains("listen") || err.contains("connected"),
            "400 monitor must name the missing listen source (or not_connected): {}",
            monitor.body
        );
        assert!(
            !mentions_unknown_provider(&err),
            "explicit github is not unknown provider: {}",
            monitor.body
        );
    } else {
        assert_eq!(
            monitor.status,
            StatusCode::OK,
            "monitor with explicit github and no Connect must be 200 or 400, not {} {}",
            monitor.status,
            monitor.body
        );
        if let Some(tickets) = monitor.body.get("tickets") {
            let arr = tickets.as_array().unwrap_or_else(|| {
                panic!("200 monitor tickets[] must be an array: {}", monitor.body)
            });
            assert!(
                arr.is_empty(),
                "200 monitor must not include tickets[] from a public issues list: {}",
                monitor.body
            );
        }
        let poll_or_err = format!(
            "{} {}",
            monitor.body.get("poll").unwrap_or(&json!(null)),
            monitor.body.get("last_error").unwrap_or(&json!(null))
        )
        .to_ascii_lowercase();
        if !poll_or_err.trim().is_empty() && poll_or_err != "null null" {
            assert!(
                poll_or_err.contains("not_connected")
                    || poll_or_err.contains("not connected")
                    || poll_or_err.contains("skipped")
                    || poll_or_err.contains("disabled"),
                "200 poll/last_error may say not_connected (must not look like a GitHub GET): {}",
                monitor.body
            );
        }
    }

    let poll = call_inbox_disconnected_timed(
        &app,
        "POST",
        &poll_path,
        Some(&jar),
        Some(json!({})),
        "POST /inbox/poll after disconnected monitor",
    )
    .await;
    assert_eq!(
        poll.status,
        StatusCode::OK,
        "POST /inbox/poll after disconnected monitor must be 200 skipped/not_connected/disabled, not {}: {}",
        poll.status,
        poll.body
    );
    assert_inbox_raw_omits_access_token(&poll, "POST /inbox/poll disconnected");
    assert_inbox_did_not_open_github(&poll, "POST /inbox/poll disconnected");
    assert!(
        inbox_poll_says_skipped_disconnected_or_disabled(&poll),
        "200 poll must say skipped / not_connected / disabled: {}",
        poll.body
    );
    assert!(
        poll.body.get("run_id").is_none() || poll.body["run_id"].is_null(),
        "poll must not include a run_id: {}",
        poll.body
    );
    if let Some(tickets) = poll.body.get("tickets") {
        let arr = tickets
            .as_array()
            .unwrap_or_else(|| panic!("poll tickets[] must be an array: {}", poll.body));
        assert!(
            arr.is_empty(),
            "poll must not include tickets[] from a public issues list: {}",
            poll.body
        );
    }

    let after = call(&app, "GET", &inbox, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET after poll: {}",
        after.body
    );
    assert_eq!(
        inbox_ticket_snapshot(&after.body),
        snap_tickets,
        "GET /inbox tickets must stay the snapshot: before={} after={}",
        snap.body,
        after.body
    );
    assert!(
        inbox_github_issue_keys(&after.body).is_empty(),
        "GET /inbox must not grow an owner/repo#n row from an anonymous issues list: {}",
        after.body
    );
    assert_inbox_raw_omits_access_token(&after, "GET /inbox after disconnected poll");
    assert_inbox_did_not_open_github(&after, "GET /inbox after disconnected poll");

    let runs = call(&app, "GET", &fleet(&ws, "/runs"), Some(&jar), None).await;
    assert_eq!(
        runs.status,
        StatusCode::OK,
        "GET runs after poll: {}",
        runs.body
    );
    assert!(
        inbox_fleet_inbox_source_runs(&runs.body).is_empty(),
        "GET /fleet/runs must have no new inbox-source run after auto_launch monitor/poll with no Connect: {}",
        runs.body
    );
}

#[tokio::test]
async fn inbox_unauthenticated_get_is_401() {
    let app = app().await;
    let (_email, _jar, ws) = signup_and_login(&app).await;

    let out = call(&app, "GET", &inbox_path(&ws, ""), None, None).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
}

fn hosts_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/worker-hosts")
}

fn members_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/members")
}

fn invites_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/invites")
}

fn json_string_secrets(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) if !s.is_empty() => out.push(s.clone()),
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut out);
    out
}

fn assert_unauthenticated_leaks_none(out: &HttpOut, secrets: &[&str], label: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{label} must be 401 not authenticated: {} {}",
        out.status,
        out.body
    );
    let err = http_error_text(out).to_ascii_lowercase();
    assert!(
        err.contains("not authenticated"),
        "{label} must say not authenticated: {}",
        out.body
    );
    assert!(
        out.body.get("run_id").is_none() || out.body["run_id"].is_null(),
        "{label} must not return a run_id: {}",
        out.body
    );
    assert_ne!(
        out.body.get("ok"),
        Some(&json!(true)),
        "{label} must not return ok:true: {}",
        out.body
    );
    for key in ["tickets", "hosts", "members", "invites", "runs"] {
        if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
            assert!(
                arr.is_empty(),
                "{label} must not include {key}: {}",
                out.body
            );
        }
    }
    let hay = format!("{} {}", out.raw, out.body);
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        assert!(
            !hay.contains(secret),
            "{label} must not leak {secret:?}: {}",
            out.body
        );
    }
}

/// No-cookie inbox mutate and hosts/members GET must 401 and must not leak
/// tickets, inventory, or emails. Anon finish/monitor must not write.
#[tokio::test]
async fn unauthenticated_inbox_hosts_members_are_401() {
    use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox};

    let (app, state) = app_with_state().await;
    let (a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "A /me: {}", me.body);
    let a_user_id = me.body["user"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("A user id: {}", me.body))
        .to_string();

    let host_name = format!("pending-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &hosts_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "name": host_name })),
    )
    .await;
    assert!(
        created.status == StatusCode::OK || created.status == StatusCode::CREATED,
        "create pending host: {} {}",
        created.status,
        created.body
    );
    assert_eq!(
        created.body["host"]["status"].as_str(),
        Some("pending"),
        "create must leave a pending host: {}",
        created.body
    );
    assert_eq!(
        created.body["host"]["name"].as_str(),
        Some(host_name.as_str()),
        "pending host name: {}",
        created.body
    );
    let hid = created.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("host.id: {}", created.body))
        .to_string();
    let pair_token = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair_token: {}", created.body))
        .to_string();
    assert!(
        pair_token.starts_with("flp_"),
        "pair_token prefix: {pair_token}"
    );

    let hostname = format!("unauth-box-{}", Uuid::new_v4());
    let hw_os = format!("unauth-os-{}", Uuid::new_v4());
    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": pair_token,
            "hostname": hostname,
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let machine_token = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("machine_token: {}", paired.body))
        .to_string();
    assert!(
        !machine_token.is_empty(),
        "pair must return machine_token: {}",
        paired.body
    );
    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine_token,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "hardware": {
                "cpu_cores": 8,
                "mem_total_bytes": 32_000_000_000u64,
                "os": hw_os
            }
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat: {}", hb.body);

    let guest_email = unique_email();
    let invited = call(
        &app,
        "POST",
        &invites_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "email": guest_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        invited.status,
        StatusCode::CREATED,
        "invite unique guest: {}",
        invited.body
    );
    assert_ne!(
        invited.body.get("added_existing_user"),
        Some(&json!(true)),
        "guest must stay a pending invite: {}",
        invited.body
    );
    let invite_id = invited.body["invite"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("invite.id: {}", invited.body))
        .to_string();

    let ticket_key = format!("unauth-{}/item#1", Uuid::new_v4());
    let ticket_title = format!("A-only ticket {}", Uuid::new_v4());
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");
    let seeded = ticket_repo::upsert_inbox(
        &state.db,
        &UpsertInbox {
            workspace_id: a_ws_id,
            provider: "github".into(),
            external_key: ticket_key.clone(),
            title: ticket_title.clone(),
            url: None,
            summary: "must not leak to anonymous callers".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("seed A's inbox");
    let ticket_id = seeded.id.to_string();

    let snap_inbox = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        snap_inbox.status,
        StatusCode::OK,
        "A GET /inbox: {}",
        snap_inbox.body
    );
    let snap_ticket = inbox_ticket(&snap_inbox.body, "github", &ticket_key);
    assert_eq!(snap_ticket["id"].as_str(), Some(ticket_id.as_str()));
    assert_eq!(snap_ticket["title"].as_str(), Some(ticket_title.as_str()));

    let snap_hosts = call(&app, "GET", &hosts_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_hosts.status,
        StatusCode::OK,
        "A GET /worker-hosts: {}",
        snap_hosts.body
    );
    let hosts = snap_hosts.body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("hosts: {}", snap_hosts.body));
    let listed = hosts
        .iter()
        .find(|h| h["id"].as_str() == Some(hid.as_str()))
        .unwrap_or_else(|| panic!("created host missing: {}", snap_hosts.body));
    assert_eq!(listed["name"].as_str(), Some(host_name.as_str()));
    assert_eq!(
        listed["hostname"].as_str(),
        Some(hostname.as_str()),
        "paired hostname must be in A's list so the leak check is not vacuous: {}",
        snap_hosts.body
    );
    assert_eq!(
        listed["hardware"]["os"].as_str(),
        Some(hw_os.as_str()),
        "hardware must be in A's list so the leak check is not vacuous: {}",
        snap_hosts.body
    );

    let snap_members = call(&app, "GET", &members_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_members.status,
        StatusCode::OK,
        "A GET /members: {}",
        snap_members.body
    );
    let members = snap_members.body["members"]
        .as_array()
        .unwrap_or_else(|| panic!("members: {}", snap_members.body));
    assert!(
        members.iter().any(|m| {
            m["email"].as_str() == Some(a_email.as_str())
                && m["user_id"].as_str() == Some(a_user_id.as_str())
        }),
        "A must appear on /members: {}",
        snap_members.body
    );

    let snap_invites = call(&app, "GET", &invites_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_invites.status,
        StatusCode::OK,
        "A GET /invites: {}",
        snap_invites.body
    );
    let invites = snap_invites.body["invites"]
        .as_array()
        .unwrap_or_else(|| panic!("invites: {}", snap_invites.body));
    assert!(
        invites.iter().any(|i| {
            i["id"].as_str() == Some(invite_id.as_str())
                && i["email"].as_str() == Some(guest_email.as_str())
        }),
        "guest invite must appear: {}",
        snap_invites.body
    );

    let snap_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        snap_runs.status,
        StatusCode::OK,
        "A GET /fleet/runs: {}",
        snap_runs.body
    );

    let mut secrets: Vec<String> = vec![
        ticket_id.clone(),
        ticket_key.clone(),
        ticket_title.clone(),
        hid.clone(),
        host_name.clone(),
        hostname.clone(),
        hw_os.clone(),
        pair_token.clone(),
        machine_token.clone(),
        a_email.clone(),
        guest_email.clone(),
        a_user_id.clone(),
        invite_id.clone(),
    ];
    secrets.extend(json_string_secrets(&snap_inbox.body["tickets"]));
    if let Some(hosts) = snap_hosts.body["hosts"].as_array() {
        for h in hosts {
            for key in ["id", "name", "hostname"] {
                if let Some(v) = h.get(key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        secrets.push(v.to_string());
                    }
                }
            }
            if !h["hardware"].is_null() {
                secrets.extend(json_string_secrets(&h["hardware"]));
            }
        }
    }
    secrets.extend(json_string_secrets(&snap_members.body["members"]));
    if let Some(you_id) = snap_members.body["you"]["user_id"].as_str() {
        secrets.push(you_id.to_string());
    }
    secrets.extend(json_string_secrets(&snap_invites.body["invites"]));
    secrets.sort();
    secrets.dedup();
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();

    let anon_monitor = call(
        &app,
        "POST",
        &inbox_path(&a_ws, ""),
        None,
        Some(json!({
            "enabled": true,
            "providers": ["github"],
            "auto_launch": true,
        })),
    )
    .await;
    assert_unauthenticated_leaks_none(&anon_monitor, &secret_refs, "anon POST /inbox");

    let anon_poll = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/poll"),
        None,
        Some(json!({})),
    )
    .await;
    assert_unauthenticated_leaks_none(&anon_poll, &secret_refs, "anon POST /inbox/poll");

    let anon_finish = call(
        &app,
        "POST",
        &inbox_path(&a_ws, "/finish"),
        None,
        Some(json!({
            "provider": "github",
            "key": ticket_key,
        })),
    )
    .await;
    assert_unauthenticated_leaks_none(&anon_finish, &secret_refs, "anon POST /inbox/finish");

    let after_inbox = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        after_inbox.status,
        StatusCode::OK,
        "A GET /inbox after anon mutate: {}",
        after_inbox.body
    );
    assert_eq!(
        after_inbox.body, snap_inbox.body,
        "anon monitor/poll/finish must not change A's inbox (no auto_launch, no launched ticket): before={} after={}",
        snap_inbox.body, after_inbox.body
    );
    assert!(
        !inbox_monitor_is_enabled_auto_launch(&after_inbox.body),
        "anon monitor must not enable auto_launch: {}",
        after_inbox.body
    );
    let after_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        after_runs.status,
        StatusCode::OK,
        "A GET /fleet/runs after anon mutate: {}",
        after_runs.body
    );
    assert_eq!(
        after_runs.body, snap_runs.body,
        "anon finish must not insert a run: before={} after={}",
        snap_runs.body, after_runs.body
    );

    let anon_hosts = call(&app, "GET", &hosts_path(&a_ws), None, None).await;
    assert_unauthenticated_leaks_none(&anon_hosts, &secret_refs, "anon GET /worker-hosts");

    let anon_host = call(
        &app,
        "GET",
        &format!("{}/{}", hosts_path(&a_ws), hid),
        None,
        None,
    )
    .await;
    assert_unauthenticated_leaks_none(&anon_host, &secret_refs, "anon GET /worker-hosts/{hid}");

    let anon_members = call(&app, "GET", &members_path(&a_ws), None, None).await;
    assert_unauthenticated_leaks_none(&anon_members, &secret_refs, "anon GET /members");

    let anon_invites = call(&app, "GET", &invites_path(&a_ws), None, None).await;
    assert_unauthenticated_leaks_none(&anon_invites, &secret_refs, "anon GET /invites");

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every GET 401s: {}",
        health.body
    );

    let inbox_again = call(&app, "GET", &inbox_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        inbox_again.status,
        StatusCode::OK,
        "A cookie GET /inbox must stay 200: {}",
        inbox_again.body
    );
    let hosts_again = call(&app, "GET", &hosts_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        hosts_again.status,
        StatusCode::OK,
        "A cookie GET /worker-hosts must stay 200: {}",
        hosts_again.body
    );
    let members_again = call(&app, "GET", &members_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        members_again.status,
        StatusCode::OK,
        "A cookie GET /members must stay 200: {}",
        members_again.body
    );
    assert!(
        hosts_again.body["hosts"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|h| h["id"].as_str() == Some(hid.as_str())),
        "control GET must still list A's host: {}",
        hosts_again.body
    );
    assert!(
        members_again.body["members"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| m["email"].as_str() == Some(a_email.as_str())),
        "control GET must still list A's email: {}",
        members_again.body
    );
}

fn personas_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/personas")
}

fn integrations_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/integrations")
}

fn response_location(out: &HttpOut) -> String {
    out.headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Verifiable `oauth_state` JWTs in a body, Location, or Set-Cookie.
fn issued_oauth_state_jwts(out: &HttpOut) -> Vec<String> {
    let set_cookies = parse_set_cookies(&out.headers)
        .into_iter()
        .map(|c| c.raw)
        .collect::<Vec<_>>()
        .join(" ");
    let hay = format!(
        "{} {} {} {}",
        out.raw,
        out.body,
        response_location(out),
        set_cookies
    );
    let mut found = Vec::new();
    for raw in hay.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
    {
        let mut parts = raw.split('.');
        let (Some(h), Some(p), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if h.len() < 8 || p.len() < 8 || s.len() < 8 {
            continue;
        }
        if verify_oauth_state(TEST_JWT_SECRET, raw).is_ok() {
            found.push(raw.to_string());
        }
    }
    found.sort();
    found.dedup();
    found
}

fn assert_unauthenticated_persona_integration_leaks_none(
    out: &HttpOut,
    secrets: &[&str],
    label: &str,
) {
    assert_unauthenticated_leaks_none(out, secrets, label);
    assert_ne!(
        out.status,
        StatusCode::FOUND,
        "{label} must not 302: {} loc={}",
        out.status,
        response_location(out)
    );
    assert_ne!(
        out.status,
        StatusCode::TEMPORARY_REDIRECT,
        "{label} must not 307 to a provider: {} loc={}",
        out.status,
        response_location(out)
    );
    let loc = response_location(out);
    let loc_l = loc.to_ascii_lowercase();
    assert!(
        loc.is_empty()
            || !(loc_l.contains("github.com")
                || loc_l.contains("gitlab.com")
                || loc.contains("state=")),
        "{label} must not 302 to GitHub or issue oauth state: {loc}"
    );
    let hay = format!("{} {}", out.raw, out.body);
    let hay_l = hay.to_ascii_lowercase();
    assert!(
        !(hay_l.contains("github.com/settings/apps/new") || hay.contains("name=\"manifest\"")),
        "{label} must not return GitHub manifest HTML: {}",
        out.raw
    );
    for key in ["oauth_start_path", "client_secret", "access_token"] {
        assert!(
            !hay.contains(key),
            "{label} must not include {key}: {}",
            out.body
        );
    }
    assert_ne!(
        out.body.get("configured"),
        Some(&json!(true)),
        "{label} must not be 200 configured: {}",
        out.body
    );
    if let Some(arr) = out.body.get("personas").and_then(|v| v.as_array()) {
        assert!(
            arr.is_empty(),
            "{label} must not include personas: {}",
            out.body
        );
    }
    if let Some(arr) = out.body.get("integrations").and_then(|v| v.as_array()) {
        assert!(
            arr.is_empty(),
            "{label} must not include integrations catalog: {}",
            out.body
        );
    }
    assert_ne!(
        out.body.get("accepted"),
        Some(&json!(true)),
        "{label} must not stamp partners disclaimer: {}",
        out.body
    );
    let issued = issued_oauth_state_jwts(out);
    assert!(
        issued.is_empty(),
        "{label} must not issue an oauth state nonce JWT: {issued:?} body={}",
        out.body
    );
}

async fn count_oauth_state_nonces(pool: &fleet_db::PgPool, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM integration_oauth_state_nonces
        WHERE workspace_id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("count oauth state nonces")
}

fn chat_status_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/status")
}

fn chat_stream_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/stream")
}

/// Docs §3.1/§8.1: cookie APIs without cookies are 401 not authenticated.
/// Fleet/chat/cases must not leak board titles, spec_md, run ids, session
/// work_dir, conversation history, or a provider inventory.
fn assert_unauthenticated_fleet_chat_cases_leaks_none(
    out: &HttpOut,
    secrets: &[&str],
    label: &str,
) {
    assert_unauthenticated_leaks_none(out, secrets, label);
    for key in [
        "tasks",
        "conversations",
        "cases",
        "sessions",
        "sandboxes",
        "workers",
        "providers",
        "models",
        "messages",
        "chat",
        "activity",
        "waiting",
    ] {
        if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
            assert!(
                arr.is_empty(),
                "{label} must not include {key}: {}",
                out.body
            );
        }
    }
    assert!(
        out.body.get("conversation").is_none() || out.body["conversation"].is_null(),
        "{label} must not include conversation: {}",
        out.body
    );
    assert!(
        out.body.get("tick_id").is_none() || out.body["tick_id"].is_null(),
        "{label} must not return a tick_id: {}",
        out.body
    );
    assert!(
        out.body.get("spawned").is_none() || out.body["spawned"].is_null(),
        "{label} must not return spawned (including spawned=0): {}",
        out.body
    );
    assert!(
        out.body.get("plans_materialized").is_none() || out.body["plans_materialized"].is_null(),
        "{label} must not return plans_materialized: {}",
        out.body
    );
    assert!(
        out.body.get("focus_run_id").is_none() || out.body["focus_run_id"].is_null(),
        "{label} must not return focus_run_id: {}",
        out.body
    );
    assert_ne!(
        out.body.get("available"),
        Some(&json!(true)),
        "{label} must not return a provider inventory (available=true): {}",
        out.body
    );
    assert_ne!(
        out.body.get("enabled"),
        Some(&json!(true)),
        "{label} must not return a provider inventory (enabled=true): {}",
        out.body
    );
    if let Some(url) = out.body.get("base_url").and_then(|v| v.as_str()) {
        assert!(
            url.is_empty(),
            "{label} must not return a provider base_url: {}",
            out.body
        );
    }
    if let Some(model) = out.body.get("default_model").and_then(|v| v.as_str()) {
        assert!(
            model.is_empty(),
            "{label} must not return default_model: {}",
            out.body
        );
    }
    match out.body.get("work_dir") {
        None => {}
        Some(Value::Null) => {}
        Some(Value::String(s)) if s.is_empty() => {}
        Some(_) => panic!("{label} must not return work_dir: {}", out.body),
    }
}

/// Docs §3.1/§8.1: no-cookie fleet / chat / cases must 401 and must not leak
/// board, messages, or titles. Distinct from `fleet_requires_membership` /
/// `chat_stream_other_workspace_forbidden` / cases other-ws (logged-in 403).
/// If `require_user` is dropped those still 403 via `require_member` and an
/// anonymous GET /fleet/tasks can leak spec_md.
/// Keep `unauthenticated_cookie_routes_are_401` and
/// `inbox_unauthenticated_get_is_401`. Filter `-- unauthenticated_fleet_chat_cases`.
#[tokio::test]
async fn unauthenticated_fleet_chat_cases() {
    let spec_md_root =
        "# Root task\n\nWrite a short plan artifact, then succeed so `t2` can run.\n";
    let spec_md_child = "# Child task\n\nDepends on `t1`. Produce a final summary artifact.\n";

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");

    let fx = call(
        &app,
        "POST",
        &fleet(&a_ws, "/runs/fixture"),
        Some(&a_jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "A POST /runs/fixture: {}",
        fx.body
    );
    let run_id = fx.body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("fixture run_id: {}", fx.body))
        .to_string();
    let tick = call(&app, "POST", &fleet(&a_ws, "/tick"), Some(&a_jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "A POST /tick: {}", tick.body);

    let conv_title = format!("unauth-secret-conv-{}", Uuid::new_v4());
    let stored_history = format!("unauth-secret-msg-{}", Uuid::new_v4());
    let created_conv = call(
        &app,
        "POST",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "title": conv_title })),
    )
    .await;
    assert_eq!(
        created_conv.status,
        StatusCode::CREATED,
        "A POST /chat/conversations: {}",
        created_conv.body
    );
    let cid = created_conv.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created_conv.body))
        .to_string();
    let cid_uuid = Uuid::parse_str(&cid).expect("conversation uuid");
    fleet_db::repos::chats::insert_message(
        &state.db,
        Uuid::now_v7(),
        cid_uuid,
        "user",
        &stored_history,
        None,
        None,
        &json!([]),
        chrono::Utc::now(),
    )
    .await
    .expect("plant stored user turn on A's thread");

    let case_title = format!("unauth-secret-case-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &a_jar, &a_ws, &case_title).await;

    let snap_tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "A cookie GET /fleet/tasks: {}",
        snap_tasks.body
    );
    let task_titles: Vec<String> = snap_tasks.body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks: {}", snap_tasks.body))
        .iter()
        .filter_map(|t| t["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        task_titles.iter().any(|t| t == "Root task")
            && task_titles.iter().any(|t| t == "Child task"),
        "cookie GET /fleet/tasks must list fixture titles so the leak check is not vacuous: {task_titles:?} {}",
        snap_tasks.body
    );
    let spec_md = snap_tasks.body["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["spec_md"].as_str())
        .find(|s| !s.is_empty())
        .unwrap_or(spec_md_root)
        .to_string();

    let snap_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        snap_runs.status,
        StatusCode::OK,
        "A cookie GET /fleet/runs: {}",
        snap_runs.body
    );
    assert!(
        fleet_run_ids(&snap_runs.body)
            .iter()
            .any(|id| id == &run_id),
        "cookie GET /runs must list fixture run {run_id}: {}",
        snap_runs.body
    );

    let snap_sessions = call(&app, "GET", &fleet(&a_ws, "/sessions"), Some(&a_jar), None).await;
    assert_eq!(
        snap_sessions.status,
        StatusCode::OK,
        "A cookie GET /fleet/sessions: {}",
        snap_sessions.body
    );
    let work_dirs: Vec<String> = snap_sessions.body["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["work_dir"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let snap_convs = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap_convs.status,
        StatusCode::OK,
        "A cookie GET /chat/conversations: {}",
        snap_convs.body
    );
    assert!(
        snap_convs.body["conversations"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(cid.as_str())
                && c["title"].as_str() == Some(conv_title.as_str())),
        "cookie GET must list the secret conversation: {}",
        snap_convs.body
    );

    let snap_one_conv = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap_one_conv.status,
        StatusCode::OK,
        "A cookie GET /chat/conversations/{{cid}}: {}",
        snap_one_conv.body
    );
    assert!(
        conversation_message_contents(&snap_one_conv.body)
            .iter()
            .any(|c| c == &stored_history),
        "snapshot must include planted stored history so the leak check is not vacuous: {}",
        snap_one_conv.body
    );

    let snap_cases = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        snap_cases.status,
        StatusCode::OK,
        "A cookie GET /cases: {}",
        snap_cases.body
    );
    assert!(
        snap_cases.body["cases"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(case_id.as_str())
                && c["title"].as_str() == Some(case_title.as_str())),
        "cookie GET must list the secret case: {}",
        snap_cases.body
    );

    let mut secrets: Vec<String> = vec![
        "Root task".into(),
        "Child task".into(),
        spec_md,
        spec_md_root.into(),
        spec_md_child.into(),
        run_id.clone(),
        conv_title.clone(),
        cid.clone(),
        stored_history.clone(),
        case_title.clone(),
        case_id.clone(),
    ];
    secrets.extend(work_dirs);
    secrets.extend(json_string_secrets(&snap_tasks.body["tasks"]));
    secrets.extend(fleet_run_ids(&snap_runs.body));
    secrets.sort();
    secrets.dedup();
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();

    // 2. No-cookie fleet board / tick / fixture.
    let fleet_probes: Vec<(&str, String, Option<Value>, &str)> = vec![
        (
            "GET",
            fleet(&a_ws, ""),
            None,
            "anon GET /api/workspaces/{A}/fleet",
        ),
        ("GET", fleet(&a_ws, "/tasks"), None, "anon GET /fleet/tasks"),
        ("GET", fleet(&a_ws, "/runs"), None, "anon GET /fleet/runs"),
        (
            "GET",
            fleet(&a_ws, "/sessions"),
            None,
            "anon GET /fleet/sessions",
        ),
        (
            "GET",
            fleet(&a_ws, "/workers"),
            None,
            "anon GET /fleet/workers",
        ),
        (
            "GET",
            fleet(&a_ws, "/overview"),
            None,
            "anon GET /fleet/overview",
        ),
        (
            "GET",
            fleet(&a_ws, "/settings"),
            None,
            "anon GET /fleet/settings",
        ),
        ("POST", fleet(&a_ws, "/tick"), None, "anon POST /fleet/tick"),
        (
            "POST",
            fleet(&a_ws, "/runs/fixture"),
            Some(json!({ "kind": "chain" })),
            "anon POST /runs/fixture {kind: chain}",
        ),
    ];
    for (method, uri, body, label) in fleet_probes {
        let out = call(&app, method, &uri, None, body).await;
        assert_unauthenticated_fleet_chat_cases_leaks_none(&out, &secret_refs, label);
    }

    let after_tasks = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        after_tasks.status,
        StatusCode::OK,
        "A GET /tasks after anon tick/fixture: {}",
        after_tasks.body
    );
    assert_eq!(
        after_tasks.body, snap_tasks.body,
        "anon tick/fixture must not mutate GET /tasks: before={} after={}",
        snap_tasks.body, after_tasks.body
    );
    let after_runs = call(&app, "GET", &fleet(&a_ws, "/runs"), Some(&a_jar), None).await;
    assert_eq!(
        after_runs.status,
        StatusCode::OK,
        "A GET /runs after anon tick/fixture: {}",
        after_runs.body
    );
    assert_eq!(
        after_runs.body, snap_runs.body,
        "anon tick/fixture must not mutate GET /runs: before={} after={}",
        snap_runs.body, after_runs.body
    );

    // 3. No-cookie chat status / conversations / complete / stream.
    let chat_msg = format!("anon-must-not-enqueue-{}", Uuid::new_v4());
    let chat_body = json!({
        "messages": [{ "role": "user", "content": chat_msg }],
        "conversation_id": cid,
        "provider": "ollama",
    });
    let chat_probes: Vec<(&str, String, Option<Value>, &str)> = vec![
        (
            "GET",
            chat_status_path(&a_ws),
            None,
            "anon GET /chat/status",
        ),
        (
            "GET",
            chat_conversations_path(&a_ws),
            None,
            "anon GET /chat/conversations",
        ),
        (
            "GET",
            chat_conversation_path(&a_ws, &cid),
            None,
            "anon GET /chat/conversations/{cid}",
        ),
        (
            "POST",
            chat_complete_path(&a_ws),
            Some(chat_body.clone()),
            "anon POST /chat {messages}",
        ),
        (
            "POST",
            chat_stream_path(&a_ws),
            Some(chat_body),
            "anon POST /chat/stream",
        ),
    ];
    for (method, uri, body, label) in chat_probes {
        let out = call(&app, method, &uri, None, body).await;
        assert_unauthenticated_fleet_chat_cases_leaks_none(&out, &secret_refs, label);
        assert!(
            !out.raw.contains(&chat_msg) && !out.body.to_string().contains(&chat_msg),
            "{label} must not echo the new user message: {}",
            out.body
        );
    }
    assert_no_chat_turn(&state, a_ws_id, "anon POST /chat and /chat/stream").await;

    let after_convs = call(
        &app,
        "GET",
        &chat_conversations_path(&a_ws),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        after_convs.status,
        StatusCode::OK,
        "A GET /chat/conversations after anon chat: {}",
        after_convs.body
    );
    assert_eq!(
        after_convs.body, snap_convs.body,
        "anon chat must not insert a conversation: before={} after={}",
        snap_convs.body, after_convs.body
    );
    let after_one_conv = call(
        &app,
        "GET",
        &chat_conversation_path(&a_ws, &cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        after_one_conv.status,
        StatusCode::OK,
        "A GET /chat/conversations/{{cid}} after anon chat: {}",
        after_one_conv.body
    );
    assert_eq!(
        conversation_message_contents(&after_one_conv.body),
        conversation_message_contents(&snap_one_conv.body),
        "anon POST /chat must not append a turn: before={} after={}",
        snap_one_conv.body,
        after_one_conv.body
    );

    // 4. No-cookie cases list / get / create.
    let anon_case_title = format!("anon-must-not-insert-{}", Uuid::new_v4());
    let case_probes: Vec<(&str, String, Option<Value>, &str)> = vec![
        ("GET", cases_path(&a_ws, ""), None, "anon GET /cases"),
        (
            "GET",
            cases_path(&a_ws, &format!("/{case_id}")),
            None,
            "anon GET /cases/{id}",
        ),
        (
            "POST",
            cases_path(&a_ws, ""),
            Some(json!({ "title": anon_case_title })),
            "anon POST /cases {title: unique}",
        ),
    ];
    for (method, uri, body, label) in case_probes {
        let out = call(&app, method, &uri, None, body).await;
        assert_unauthenticated_fleet_chat_cases_leaks_none(&out, &secret_refs, label);
        assert!(
            !out.raw.contains(&anon_case_title) && !out.body.to_string().contains(&anon_case_title),
            "{label} must not echo the new case title: {}",
            out.body
        );
    }

    let after_cases = call(&app, "GET", &cases_path(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        after_cases.status,
        StatusCode::OK,
        "A GET /cases after anon POST: {}",
        after_cases.body
    );
    assert_eq!(
        after_cases.body, snap_cases.body,
        "anon POST /cases must not insert a row: before={} after={}",
        snap_cases.body, after_cases.body
    );
    assert!(
        after_cases.body["cases"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|c| c["title"].as_str() != Some(anon_case_title.as_str())),
        "anon POST /cases must not persist {anon_case_title}: {}",
        after_cases.body
    );

    // 5. Control: public /health and A's cookie /fleet still 200.
    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every GET 401s: {}",
        health.body
    );

    let fleet_again = call(&app, "GET", &fleet(&a_ws, ""), Some(&a_jar), None).await;
    assert_eq!(
        fleet_again.status,
        StatusCode::OK,
        "A cookie GET /fleet must stay 200: {}",
        fleet_again.body
    );
    let tasks_again = call(&app, "GET", &fleet(&a_ws, "/tasks"), Some(&a_jar), None).await;
    assert_eq!(
        tasks_again.status,
        StatusCode::OK,
        "A cookie GET /fleet/tasks must stay 200: {}",
        tasks_again.body
    );
    assert!(
        tasks_again.body["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|t| t["title"].as_str() == Some("Root task")),
        "control GET must still list A's secret task: {}",
        tasks_again.body
    );
}

/// No-cookie personas / integrations / partners must 401 and must not leak
/// role_prompt, catalog, oauth start, or stamp disclaimer / oauth_apps.
/// Keep `unauthenticated_cookie_routes_are_401` (GET /me, GET/POST
/// /workspaces), `unauthenticated_fleet_chat_cases` (fleet/chat/cases),
/// and `unauthenticated_inbox_hosts_members_are_401`.
#[tokio::test]
async fn unauthenticated_personas_integrations_are_401() {
    use fleet_db::repos::integrations as integ_repo;
    use fleet_db::repos::personas as persona_repo;

    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let a_ws_id = Uuid::parse_str(&a_ws).expect("A workspace id");

    let persona_name = format!("A-only persona {}", Uuid::new_v4());
    let persona_slug = format!("a-only-{}", &Uuid::new_v4().to_string()[..8]);
    let role_prompt = format!("SECRET-ROLE-PROMPT-{}", Uuid::new_v4());
    let created = call(
        &app,
        "POST",
        &personas_path(&a_ws),
        Some(&a_jar),
        Some(json!({
            "name": persona_name,
            "slug": persona_slug,
            "role_prompt": role_prompt,
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "A POST /personas: {}",
        created.body
    );
    let pid = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("persona id: {}", created.body))
        .to_string();
    assert_eq!(
        created.body["role_prompt"].as_str(),
        Some(role_prompt.as_str()),
        "snapshot source must include the secret so the leak check is not vacuous: {}",
        created.body
    );

    let snap_personas = call(&app, "GET", &personas_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_personas.status,
        StatusCode::OK,
        "A GET /personas: {}",
        snap_personas.body
    );
    let listed = snap_personas.body["personas"]
        .as_array()
        .unwrap_or_else(|| panic!("personas: {}", snap_personas.body));
    let listed_p = listed
        .iter()
        .find(|p| p["id"].as_str() == Some(pid.as_str()))
        .unwrap_or_else(|| panic!("created persona missing: {}", snap_personas.body));
    assert_eq!(listed_p["name"].as_str(), Some(persona_name.as_str()));
    assert_eq!(listed_p["role_prompt"].as_str(), Some(role_prompt.as_str()));

    let snap_one = call(
        &app,
        "GET",
        &format!("{}/{}", personas_path(&a_ws), pid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        snap_one.status,
        StatusCode::OK,
        "A GET /personas/{{pid}}: {}",
        snap_one.body
    );
    assert_eq!(snap_one.body["id"].as_str(), Some(pid.as_str()));
    assert_eq!(
        snap_one.body["role_prompt"].as_str(),
        Some(role_prompt.as_str())
    );

    let snap_integ = call(&app, "GET", &integrations_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_integ.status,
        StatusCode::OK,
        "A GET /integrations: {}",
        snap_integ.body
    );
    let github = catalog_row(&snap_integ.body, "github");
    assert_eq!(github["availability"], "available", "{github}");
    assert_eq!(
        github["oauth_start_path"],
        format!("{}/github/oauth/start", integrations_path(&a_ws)),
        "A's catalog must expose oauth_start_path so the leak check is not vacuous: {github}"
    );
    assert_eq!(
        github["connected"], false,
        "fresh workspace is disconnected: {github}"
    );

    let snap_disc = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        snap_disc.status,
        StatusCode::OK,
        "A GET /partners/disclaimer: {}",
        snap_disc.body
    );
    assert_eq!(
        snap_disc.body["accepted"], false,
        "default must be not accepted: {}",
        snap_disc.body
    );
    assert!(
        snap_disc.body["accepted_at"].is_null(),
        "default accepted_at must be null: {}",
        snap_disc.body
    );

    let nonce_before = count_oauth_state_nonces(&state.db, a_ws_id).await;
    let apps_before = integ_repo::list_oauth_apps(&state.db, a_ws_id)
        .await
        .expect("list_oauth_apps before");
    let conns_before = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("list connections before");
    assert!(
        apps_before.is_empty(),
        "fresh workspace must have no oauth_apps so persist check is not vacuous: {apps_before:?}"
    );
    assert!(
        conns_before.is_empty(),
        "fresh workspace must have no connections: {conns_before:?}"
    );

    let mut secrets: Vec<String> = vec![
        pid.clone(),
        persona_name.clone(),
        persona_slug.clone(),
        role_prompt.clone(),
    ];
    secrets.extend(json_string_secrets(&snap_personas.body["personas"]));
    secrets.sort();
    secrets.dedup();
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();

    let anon_list = call(&app, "GET", &personas_path(&a_ws), None, None).await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_list,
        &secret_refs,
        "anon GET /personas",
    );

    let anon_get = call(
        &app,
        "GET",
        &format!("{}/{}", personas_path(&a_ws), pid),
        None,
        None,
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_get,
        &secret_refs,
        "anon GET /personas/{pid}",
    );

    let anon_name = format!("anon-insert-{}", Uuid::new_v4());
    let anon_slug = format!("anon-{}", &Uuid::new_v4().to_string()[..8]);
    let anon_prompt = format!("ANON-MUST-NOT-INSERT-{}", Uuid::new_v4());
    let anon_create = call(
        &app,
        "POST",
        &personas_path(&a_ws),
        None,
        Some(json!({
            "name": anon_name,
            "slug": anon_slug,
            "role_prompt": anon_prompt,
        })),
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_create,
        &secret_refs,
        "anon POST /personas",
    );
    assert!(
        !anon_create.raw.contains(&anon_prompt),
        "anon POST /personas must not echo the new role_prompt: {}",
        anon_create.body
    );

    let anon_seed = call(
        &app,
        "POST",
        &format!("{}/seed-starters", personas_path(&a_ws)),
        None,
        None,
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_seed,
        &secret_refs,
        "anon POST /personas/seed-starters",
    );
    assert!(
        anon_seed.body.get("inserted").is_none()
            || anon_seed.body["inserted"] == json!(0)
            || anon_seed.body["inserted"].is_null(),
        "anon seed-starters must not insert: {}",
        anon_seed.body
    );

    let after_personas = call(&app, "GET", &personas_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        after_personas.status,
        StatusCode::OK,
        "A GET /personas after anon create/seed: {}",
        after_personas.body
    );
    assert_eq!(
        after_personas.body, snap_personas.body,
        "anon create/seed must not insert: before={} after={}",
        snap_personas.body, after_personas.body
    );
    let db_personas = persona_repo::list_for_workspace(&state.db, a_ws_id, Some("active"))
        .await
        .expect("list personas after anon");
    assert!(
        db_personas
            .iter()
            .all(|p| p.role_prompt != anon_prompt && p.slug != anon_slug),
        "anon POST must not persist a persona: {db_personas:?}"
    );
    assert_eq!(
        db_personas.len(),
        snap_personas.body["personas"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        "anon seed-starters must not insert starter rows: {db_personas:?}"
    );

    let anon_integ = call(&app, "GET", &integrations_path(&a_ws), None, None).await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_integ,
        &secret_refs,
        "anon GET /integrations",
    );

    let anon_start = call(
        &app,
        "GET",
        &format!("{}/github/oauth/start", integrations_path(&a_ws)),
        None,
        None,
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_start,
        &secret_refs,
        "anon GET /integrations/github/oauth/start",
    );

    let anon_del = call(
        &app,
        "DELETE",
        &format!("{}/github", integrations_path(&a_ws)),
        None,
        None,
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_del,
        &secret_refs,
        "anon DELETE /integrations/github",
    );
    assert_ne!(
        anon_del.status,
        StatusCode::NOT_FOUND,
        "anon DELETE must 401 before not-connected 404: {}",
        anon_del.body
    );

    let put_client_id = format!("anon-client-{}", Uuid::new_v4());
    let put_secret = format!("anon-secret-{}", Uuid::new_v4());
    let anon_put = call(
        &app,
        "PUT",
        &format!("{}/github/oauth-app", integrations_path(&a_ws)),
        None,
        Some(json!({
            "client_id": put_client_id,
            "client_secret": put_secret,
        })),
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_put,
        &secret_refs,
        "anon PUT /integrations/github/oauth-app",
    );
    let put_hay = format!("{} {}", anon_put.raw, anon_put.body);
    assert!(
        !put_hay.contains(&put_secret) && !put_hay.contains(&put_client_id),
        "anon PUT must not echo client_id/client_secret: {}",
        anon_put.body
    );

    let after_integ = call(&app, "GET", &integrations_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        after_integ.status,
        StatusCode::OK,
        "A GET /integrations after anon mutate: {}",
        after_integ.body
    );
    assert_eq!(
        after_integ.body, snap_integ.body,
        "anon GET/start/DELETE/PUT must not change A's catalog: before={} after={}",
        snap_integ.body, after_integ.body
    );

    let nonce_after = count_oauth_state_nonces(&state.db, a_ws_id).await;
    assert_eq!(
        nonce_after, nonce_before,
        "anon oauth/start/PUT must not issue/consume an oauth state nonce"
    );
    let apps_after = integ_repo::list_oauth_apps(&state.db, a_ws_id)
        .await
        .expect("list_oauth_apps after");
    assert!(
        apps_after.is_empty(),
        "anon PUT oauth-app must not persist oauth_apps: {apps_after:?}"
    );
    let conns_after = integ_repo::list_all_for_workspace(&state.db, a_ws_id)
        .await
        .expect("list connections after");
    assert!(
        conns_after.is_empty(),
        "anon DELETE/PUT must not persist connections: {conns_after:?}"
    );

    let anon_disc_get = call(&app, "GET", &partners_disclaimer(&a_ws), None, None).await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_disc_get,
        &secret_refs,
        "anon GET /partners/disclaimer",
    );

    let anon_disc_post = call(
        &app,
        "POST",
        &partners_disclaimer(&a_ws),
        None,
        Some(json!({})),
    )
    .await;
    assert_unauthenticated_persona_integration_leaks_none(
        &anon_disc_post,
        &secret_refs,
        "anon POST /partners/disclaimer",
    );

    let after_disc = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        after_disc.status,
        StatusCode::OK,
        "A GET /partners/disclaimer after anon POST: {}",
        after_disc.body
    );
    assert_eq!(
        after_disc.body, snap_disc.body,
        "anon POST must not stamp accepted_at: before={} after={}",
        snap_disc.body, after_disc.body
    );
    assert_eq!(
        after_disc.body["accepted"], false,
        "anon POST must leave accepted=false: {}",
        after_disc.body
    );
    assert!(
        after_disc.body["accepted_at"].is_null(),
        "anon POST must not stamp accepted_at: {}",
        after_disc.body
    );

    let health = call(&app, "GET", "/health", None, None).await;
    assert_eq!(
        health.status,
        StatusCode::OK,
        "GET /health must stay 200 so this is not a tautology that every GET 401s: {}",
        health.body
    );

    let personas_again = call(&app, "GET", &personas_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        personas_again.status,
        StatusCode::OK,
        "A cookie GET /personas must stay 200: {}",
        personas_again.body
    );
    let integ_again = call(&app, "GET", &integrations_path(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        integ_again.status,
        StatusCode::OK,
        "A cookie GET /integrations must stay 200: {}",
        integ_again.body
    );
    let disc_again = call(&app, "GET", &partners_disclaimer(&a_ws), Some(&a_jar), None).await;
    assert_eq!(
        disc_again.status,
        StatusCode::OK,
        "A cookie GET /partners/disclaimer must stay 200: {}",
        disc_again.body
    );
    assert!(
        personas_again.body["personas"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| {
                p["id"].as_str() == Some(pid.as_str())
                    && p["role_prompt"].as_str() == Some(role_prompt.as_str())
            }),
        "control GET must still list A's secret persona: {}",
        personas_again.body
    );
    assert_eq!(
        catalog_row(&integ_again.body, "github")["oauth_start_path"],
        format!("{}/github/oauth/start", integrations_path(&a_ws)),
        "control GET must still list oauth_start_path: {}",
        integ_again.body
    );
}

/// Docs §3.1/§3.4 / fleet-auth README: cookie BFF is `fleet_access` +
/// `fleet_refresh`. `require_user` reads only ACCESS_COOKIE then
/// `is_session_active`. A stolen access JWT in `Authorization: Bearer`
/// (no Cookie) must not unlock fleet/chat/cases. Dual-auth that decodes
/// Bearer without the cookie would keep a CSRF-copied JWT valid.
/// Keep `unauthenticated_cookie_routes_are_401` and
/// `access_jwt_rejected_as_refresh`.
#[tokio::test]
async fn bearer_access_not_cookie_cannot_authenticate_fleet_chat_cases() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    assert_eq!(
        access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {access:?}"
    );

    // 4. Control: Cookie (no Authorization) POST /fleet/runs/fixture 201.
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "cookie POST /fleet/runs/fixture must still 201: {}",
        fx.body
    );
    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(tick.status, StatusCode::OK, "cookie tick: {}", tick.body);

    let conv_title = format!("bearer-secret-conv-{}", Uuid::new_v4());
    let created_conv = call(
        &app,
        "POST",
        &chat_conversations_path(&ws),
        Some(&jar),
        Some(json!({ "title": conv_title })),
    )
    .await;
    assert_eq!(
        created_conv.status,
        StatusCode::CREATED,
        "cookie POST /chat/conversations: {}",
        created_conv.body
    );
    let cid = created_conv.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created_conv.body))
        .to_string();

    let case_title = format!("bearer-secret-case-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &jar, &ws, &case_title).await;

    let cookie_fleet = call(&app, "GET", &fleet(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        cookie_fleet.status,
        StatusCode::OK,
        "cookie GET /fleet must 200 so this is not every GET 401: {}",
        cookie_fleet.body
    );
    let cookie_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        cookie_tasks.status,
        StatusCode::OK,
        "cookie GET /fleet/tasks: {}",
        cookie_tasks.body
    );
    let task_titles: Vec<String> = cookie_tasks.body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks: {}", cookie_tasks.body))
        .iter()
        .filter_map(|t| t["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        task_titles.iter().any(|t| t == "Root task")
            && task_titles.iter().any(|t| t == "Child task"),
        "cookie GET /fleet/tasks must list fixture titles: {task_titles:?} {}",
        cookie_tasks.body
    );
    let spec_md = cookie_tasks.body["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["spec_md"].as_str())
        .find(|s| !s.is_empty())
        .unwrap_or("# Root task")
        .to_string();

    let cookie_convs = call(&app, "GET", &chat_conversations_path(&ws), Some(&jar), None).await;
    assert_eq!(
        cookie_convs.status,
        StatusCode::OK,
        "cookie GET /chat/conversations: {}",
        cookie_convs.body
    );
    assert!(
        cookie_convs.body["conversations"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(cid.as_str())
                && c["title"].as_str() == Some(conv_title.as_str())),
        "cookie GET must list the secret conversation: {}",
        cookie_convs.body
    );

    let cookie_cases = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        cookie_cases.status,
        StatusCode::OK,
        "cookie GET /cases: {}",
        cookie_cases.body
    );
    assert!(
        cookie_cases.body["cases"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(case_id.as_str())
                && c["title"].as_str() == Some(case_title.as_str())),
        "cookie GET must list the secret case: {}",
        cookie_cases.body
    );

    let secrets = [
        "Root task",
        "Child task",
        spec_md.as_str(),
        conv_title.as_str(),
        cid.as_str(),
        case_title.as_str(),
        case_id.as_str(),
    ];

    // 2. Same Bearer (no Cookie) on fleet / tasks / conversations / cases.
    for (uri, label) in [
        (fleet(&ws, ""), "GET /api/workspaces/{A}/fleet"),
        (fleet(&ws, "/tasks"), "GET /fleet/tasks"),
        (chat_conversations_path(&ws), "GET /chat/conversations"),
        (cases_path(&ws, ""), "GET /cases"),
    ] {
        let out = call_bearer(&app, "GET", &uri, &access, None).await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "{label} Bearer must not 200 as if the cookie were present: {}",
            out.body
        );
        assert_unauthenticated_leaks_none(
            &out,
            &secrets,
            &format!("{label} Bearer <fleet_access JWT> no Cookie"),
        );
        for key in ["tasks", "conversations", "cases"] {
            if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
                assert!(
                    arr.is_empty(),
                    "{label} Bearer must not include {key}: {}",
                    out.body
                );
            }
        }
        assert!(
            out.body.get("user").is_none() && out.body.get("workspaces").is_none(),
            "{label} Bearer must not include user/workspaces: {}",
            out.body
        );
    }
}

/// Cookie-BFF surfaces that `require_user` gates. A host `flm_` / `flp_`
/// Bearer (no Cookie) must 401 the same as a missing session.
async fn assert_host_token_rejected_as_cookie_bff(
    app: &axum::Router,
    token: &str,
    ws: &str,
    secrets: &[&str],
    label: &str,
) {
    let fleet_root = fleet(ws, "");
    let fleet_tasks = fleet(ws, "/tasks");
    let fleet_tick = fleet(ws, "/tick");
    let convs = chat_conversations_path(ws);
    let cases = cases_path(ws, "");
    let probes = [
        ("GET", "/api/auth/me", "GET /api/auth/me"),
        ("GET", "/api/workspaces", "GET /workspaces"),
        ("GET", fleet_root.as_str(), "GET /api/workspaces/{A}/fleet"),
        ("GET", fleet_tasks.as_str(), "GET /fleet/tasks"),
        ("GET", convs.as_str(), "GET /chat/conversations"),
        ("GET", cases.as_str(), "GET /cases"),
        ("POST", fleet_tick.as_str(), "POST /fleet/tick"),
    ];
    for (method, uri, path_label) in probes {
        let out = call_bearer(app, method, uri, token, None).await;
        let tagged = format!("{label} {path_label}");
        assert_ne!(
            out.status,
            StatusCode::OK,
            "{tagged} must not 200 as if the cookie were present: {}",
            out.body
        );
        assert_unauthenticated_leaks_none(&out, secrets, &tagged);
        for key in ["tasks", "conversations", "cases", "workspaces"] {
            if let Some(arr) = out.body.get(key).and_then(|v| v.as_array()) {
                assert!(
                    arr.is_empty(),
                    "{tagged} must not include {key}: {}",
                    out.body
                );
            }
        }
        assert!(
            out.body.get("user").is_none(),
            "{tagged} must not include user: {}",
            out.body
        );
        assert!(
            out.body.get("tick_id").is_none() || out.body["tick_id"].is_null(),
            "{tagged} must not return a tick_id: {}",
            out.body
        );
        assert!(
            out.body.get("spawned").is_none() || out.body["spawned"].is_null(),
            "{tagged} must not return spawned (including spawned=0): {}",
            out.body
        );
        assert!(
            out.body.get("plans_materialized").is_none()
                || out.body["plans_materialized"].is_null(),
            "{tagged} must not return plans_materialized: {}",
            out.body
        );
    }
}

/// Docs §3.1 / fleet-auth README: cookie BFF is `fleet_access` +
/// `fleet_refresh`. `require_user` reads only ACCESS_COOKIE then
/// `is_session_active`. Worker-host README: `flm_` is machine Bearer;
/// `flp_` is the one-shot pair body token. A stolen host token must not
/// unlock /me /fleet /chat /cases. Dual-auth that decoded Bearer `flm_` /
/// `flp_` (or hashed them against auth_sessions / users) would let anyone
/// who saw a pair panel GET spec_md and POST /tick.
/// Keep `bearer_access_not_cookie_cannot_authenticate_fleet_chat_cases`
/// and `cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease`.
#[tokio::test]
async fn machine_token_not_cookie_bff_cannot_authenticate_me_fleet_chat_cases() {
    let app = app().await;
    let (email, jar, ws) = signup_and_login(&app).await;

    // 1. Owner A POST /worker-hosts 200: snapshot flp_. POST /pair 200: flm_.
    let created = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("bff-host-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "owner POST /worker-hosts: {}",
        created.body
    );
    let used_flp = created.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("pair_token: {}", created.body))
        .to_string();
    assert!(
        used_flp.starts_with("flp_"),
        "pair_token prefix: {used_flp}"
    );
    assert!(
        !used_flp.starts_with("flm_"),
        "pair_token must not look like a machine token: {used_flp}"
    );

    let paired = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": used_flp,
            "hostname": format!("bff-box-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(paired.status, StatusCode::OK, "pair: {}", paired.body);
    let flm = paired.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("machine_token: {}", paired.body))
        .to_string();
    assert!(flm.starts_with("flm_"), "machine_token prefix: {flm}");
    assert_ne!(flm, used_flp, "machine token must not be the pair token");

    let spec_md_root =
        "# Root task\n\nWrite a short plan artifact, then succeed so `t2` can run.\n";
    let spec_md_child = "# Child task\n\nDepends on `t1`. Produce a final summary artifact.\n";
    let fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        fx.status,
        StatusCode::CREATED,
        "setup cookie POST /fleet/runs/fixture: {}",
        fx.body
    );
    let tick = call(&app, "POST", &fleet(&ws, "/tick"), Some(&jar), None).await;
    assert_eq!(
        tick.status,
        StatusCode::OK,
        "setup cookie POST /fleet/tick: {}",
        tick.body
    );

    let conv_title = format!("machine-bff-secret-conv-{}", Uuid::new_v4());
    let created_conv = call(
        &app,
        "POST",
        &chat_conversations_path(&ws),
        Some(&jar),
        Some(json!({ "title": conv_title })),
    )
    .await;
    assert_eq!(
        created_conv.status,
        StatusCode::CREATED,
        "cookie POST /chat/conversations: {}",
        created_conv.body
    );
    let cid = created_conv.body["conversation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("conversation.id: {}", created_conv.body))
        .to_string();

    let case_title = format!("machine-bff-secret-case-{}", Uuid::new_v4());
    let case_id = create_open_case(&app, &jar, &ws, &case_title).await;

    let snap_me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        snap_me.status,
        StatusCode::OK,
        "A cookie GET /me: {}",
        snap_me.body
    );
    assert_eq!(
        snap_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "cookie GET /me must include A's email so the leak check is not vacuous: {}",
        snap_me.body
    );

    let snap_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        snap_tasks.status,
        StatusCode::OK,
        "A cookie GET /fleet/tasks: {}",
        snap_tasks.body
    );
    let task_titles: Vec<String> = snap_tasks.body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("tasks: {}", snap_tasks.body))
        .iter()
        .filter_map(|t| t["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        task_titles.iter().any(|t| t == "Root task")
            && task_titles.iter().any(|t| t == "Child task"),
        "cookie GET /fleet/tasks must list fixture titles: {task_titles:?} {}",
        snap_tasks.body
    );
    let spec_md = snap_tasks.body["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["spec_md"].as_str())
        .find(|s| !s.is_empty())
        .unwrap_or(spec_md_root)
        .to_string();

    let snap_convs = call(&app, "GET", &chat_conversations_path(&ws), Some(&jar), None).await;
    assert_eq!(
        snap_convs.status,
        StatusCode::OK,
        "A cookie GET /chat/conversations: {}",
        snap_convs.body
    );
    assert!(
        snap_convs.body["conversations"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(cid.as_str())
                && c["title"].as_str() == Some(conv_title.as_str())),
        "cookie GET must list the secret conversation: {}",
        snap_convs.body
    );

    let snap_cases = call(&app, "GET", &cases_path(&ws, ""), Some(&jar), None).await;
    assert_eq!(
        snap_cases.status,
        StatusCode::OK,
        "A cookie GET /cases: {}",
        snap_cases.body
    );
    assert!(
        snap_cases.body["cases"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["id"].as_str() == Some(case_id.as_str())
                && c["title"].as_str() == Some(case_title.as_str())),
        "cookie GET must list the secret case: {}",
        snap_cases.body
    );

    let secrets = [
        email.as_str(),
        "Root task",
        "Child task",
        spec_md.as_str(),
        spec_md_root,
        spec_md_child,
        conv_title.as_str(),
        cid.as_str(),
        case_title.as_str(),
        case_id.as_str(),
    ];

    // 2. Authorization: Bearer <flm_> and no Cookie on cookie-BFF paths.
    assert_host_token_rejected_as_cookie_bff(
        &app,
        &flm,
        &ws,
        &secrets,
        "Authorization: Bearer <flm_> no Cookie",
    )
    .await;

    let after_flm_tasks = call(&app, "GET", &fleet(&ws, "/tasks"), Some(&jar), None).await;
    assert_eq!(
        after_flm_tasks.status,
        StatusCode::OK,
        "A cookie GET /tasks after flm_ probes: {}",
        after_flm_tasks.body
    );
    assert_eq!(
        after_flm_tasks.body, snap_tasks.body,
        "flm_ POST /tick must not spawn: snap={} after={}",
        snap_tasks.body, after_flm_tasks.body
    );

    // 3. Bearer <used flp_> and a fresh unused flp_ on the same paths.
    let second = call(
        &app,
        "POST",
        &hosts_path(&ws),
        Some(&jar),
        Some(json!({ "name": format!("bff-unused-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "second POST /worker-hosts: {}",
        second.body
    );
    let unused_hid = second.body["host"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("second host.id: {}", second.body))
        .to_string();
    let unused_flp = second.body["pair_token"]
        .as_str()
        .unwrap_or_else(|| panic!("second pair_token: {}", second.body))
        .to_string();
    assert!(
        unused_flp.starts_with("flp_"),
        "unused pair_token prefix: {unused_flp}"
    );
    assert_ne!(unused_flp, used_flp, "second create must mint a new flp_");

    assert_host_token_rejected_as_cookie_bff(
        &app,
        &used_flp,
        &ws,
        &secrets,
        "Authorization: Bearer <used flp_> no Cookie",
    )
    .await;
    assert_host_token_rejected_as_cookie_bff(
        &app,
        &unused_flp,
        &ws,
        &secrets,
        "Authorization: Bearer <unused flp_> no Cookie",
    )
    .await;

    let unused_pair = call(
        &app,
        "POST",
        "/api/worker-hosts/pair",
        None,
        Some(json!({
            "token": unused_flp,
            "hostname": format!("bff-unused-box-{}", Uuid::new_v4()),
            "platform": "linux-x86_64",
            "advertised_host": true,
            "advertised_container": false
        })),
    )
    .await;
    assert_eq!(
        unused_pair.status,
        StatusCode::OK,
        "unused flp_ POST /pair after cookie-BFF probes must still 200 (must not consume it): {}",
        unused_pair.body
    );
    assert_eq!(
        unused_pair.body["host_id"].as_str(),
        Some(unused_hid.as_str()),
        "unused pair must bind the second host: {}",
        unused_pair.body
    );
    let unused_flm = unused_pair.body["machine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("unused pair machine_token: {}", unused_pair.body))
        .to_string();
    assert!(
        unused_flm.starts_with("flm_"),
        "unused pair machine_token: {unused_flm}"
    );

    // 4. Control: original Cookie still GET /me 200; cookie fixture 201;
    // the same flm_ still 200s POST /me/heartbeat (not every Bearer 401s).
    let control_me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        control_me.status,
        StatusCode::OK,
        "cookie GET /me must still 200: {}",
        control_me.body
    );
    assert_eq!(
        control_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "cookie GET /me must still be A: {}",
        control_me.body
    );

    let control_fx = call(
        &app,
        "POST",
        &fleet(&ws, "/runs/fixture"),
        Some(&jar),
        Some(json!({ "kind": "chain" })),
    )
    .await;
    assert_eq!(
        control_fx.status,
        StatusCode::CREATED,
        "cookie POST /fleet/runs/fixture must still 201: {}",
        control_fx.body
    );

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &flm,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "same flm_ POST /api/worker-hosts/me/heartbeat must still 200 (not a tautology that every Bearer 401s): {}",
        hb.body
    );
    assert!(
        hb.body.get("host").is_some(),
        "control heartbeat must include host: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["status"], "online",
        "control heartbeat host must be online: {}",
        hb.body
    );
}
