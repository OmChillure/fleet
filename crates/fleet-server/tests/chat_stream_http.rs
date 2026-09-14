//! Fail-closed HTTP tests for workspace chat (`/chat`, `/chat/stream`, `/chat/status`).
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
    AppState, ClaudeConfig, GrokConfig, OllamaConfig, GeminiConfig, OpenaiConfig, ServerConfig, build_http_app,
    build_router,
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

/// Plane grok/ollama on so POST /chat and /chat/stream reach
/// `require_taking` / `run_on_runner` instead of
/// `Grok/Ollama chat is disabled`. Dummy `base_url` is enough.
async fn app_with_plane_chat() -> (axum::Router, AppState) {
    let (_base, mut state) = app_with_state().await;
    state.ollama.enabled = true;
    state.ollama.base_url = "http://127.0.0.1:9".into();
    state.grok.enabled = true;
    let app = build_router().with_state(state.clone());
    (app, state)
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

fn chat_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat")
}

fn status_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/status")
}

fn stream_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/stream")
}

fn conversations_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations")
}

fn conversation_path(ws: &str, cid: &str) -> String {
    format!("/api/workspaces/{ws}/chat/conversations/{cid}")
}

fn hosts_path(ws: &str) -> String {
    format!("/api/workspaces/{ws}/worker-hosts")
}

async fn call_bearer(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> HttpOut {
    call_inner(app, method, uri, None, Some(token), body).await
}

async fn call_timed(
    app: &axum::Router,
    method: &str,
    uri: &str,
    jar: &CookieJar,
    body: Option<Value>,
) -> HttpOut {
    tokio::time::timeout(FAIL_CLOSED, call(app, method, uri, Some(jar), body))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{method} {uri} hung for more than {FAIL_CLOSED:?} — must not enqueue a chat_turn or start the 240s wait"
            )
        })
}

/// Owner cookie creates a paired, heartbeating runner so a missing membership /
/// empty-body gate would hit `run_on_runner` and wait 240s.
/// Unique hostname so a second host is not reclaimed via hostname+platform.
async fn create_paired_online_runner(app: &axum::Router, jar: &CookieJar, ws: &str) -> Uuid {
    pair_and_heartbeat(
        app,
        jar,
        ws,
        &format!("chat-runner-{}", Uuid::new_v4()),
        None,
    )
    .await
}

/// Pair + heartbeat. `hostname` is unique per `name` so a second host in the
/// same workspace is not reclaimed via hostname+platform.
async fn pair_and_heartbeat(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    name: &str,
    agents: Option<Value>,
) -> Uuid {
    pair_and_heartbeat_machine(app, jar, ws, name, agents)
        .await
        .0
}

/// Same as [`pair_and_heartbeat`], plus the real `flm_` so later heartbeats
/// can report `current_jobs` (at-cap chat must not enqueue).
async fn pair_and_heartbeat_machine(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    name: &str,
    agents: Option<Value>,
) -> (Uuid, String) {
    let created = call(
        app,
        "POST",
        &hosts_path(ws),
        Some(jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create host {name}: {}",
        created.body
    );
    let pair_token = created.body["pair_token"]
        .as_str()
        .expect("pair_token")
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
        .expect("host_id")
        .to_string();
    let machine = paired.body["machine_token"]
        .as_str()
        .expect("machine_token")
        .to_string();
    let mut hb_body = json!({
        "advertised_host": true,
        "advertised_container": false,
        "current_jobs": 0,
    });
    if let Some(agents) = agents {
        hb_body["agents"] = agents;
    }
    let hb = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(hb_body),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat {name}: {}", hb.body);
    (Uuid::parse_str(&host_id).expect("host uuid"), machine)
}

fn planted_ready_agents(grok_model: &str, ollama_model: &str) -> Value {
    json!([
        {
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "models": [grok_model]
        },
        {
            "id": "ollama",
            "name": "Ollama",
            "installed": true,
            "logged_in": true,
            "models": [ollama_model]
        }
    ])
}

fn assert_membership_forbidden(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} must be 403 membership, not a hang/stream/enqueue: {} {}",
        out.status,
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("member"),
        "{what} 403 must be a membership error: {}",
        out.body
    );
    assert!(
        !out.raw.contains("\"type\":\"done\""),
        "{what} must not stream a turn: {}",
        out.raw
    );
}

fn assert_unauthenticated(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{what} must be 401 (require_user), not 403 membership or a hang: {} {}",
        out.status,
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        !err.contains("member"),
        "{what} 401 must not be a membership error: {}",
        out.body
    );
    assert!(
        !out.raw.contains("\"type\":\"done\""),
        "{what} must not stream a turn: {}",
        out.raw
    );
}

fn assert_empty_bad_request(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 before the runner wait: {} {}",
        out.status,
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("empty") || err.contains("message"),
        "{what} 400 should name empty/messages: {}",
        out.body
    );
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

/// NDJSON lines or `text/event-stream` `data:` payloads.
fn stream_events(raw: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            events.push(v);
        }
    }
    events
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

/// Stream must not complete a turn against another workspace's / teammate's
/// `conversation_id`. 404 before SSE, or 200 whose events are only `type=error`.
/// HTTP 200 + `type=done` / token / assistant text, or echoing stored secrets,
/// is a fail. Membership 403 is the other-ws cookie card — not this lock.
fn assert_stream_conversation_fail_closed(out: &HttpOut, secrets: &[&str], what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    if out.status == StatusCode::NOT_FOUND {
        let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
        assert!(
            err.contains("conversation"),
            "{what} 404 must name conversation: {}",
            out.body
        );
    } else {
        assert_eq!(
            out.status,
            StatusCode::OK,
            "{what} must be 404 conversation or 200 stream type=error only, not {} {}",
            out.status,
            out.body
        );
        let ctype = out
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ctype.contains("event-stream") || ctype.contains("ndjson"),
            "{what} 200 must be text/event-stream or ndjson, not {ctype}: {}",
            out.raw
        );
        let events = stream_events(&out.raw);
        assert!(
            !events.is_empty(),
            "{what} 200 stream must emit events: {}",
            out.raw
        );
        for ev in &events {
            let ty = ev.get("type").and_then(Value::as_str).unwrap_or("");
            assert_eq!(
                ty, "error",
                "{what} 200 stream events must be only type=error (no done/token/assistant), got `{ty}`: {}",
                out.raw
            );
        }
    }
    let raw_lc = out.raw.to_ascii_lowercase();
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream type=done: {}",
        out.raw
    );
    assert!(
        !raw_lc.contains("\"type\":\"token\"") && !raw_lc.contains("\"type\": \"token\""),
        "{what} must not stream token text: {}",
        out.raw
    );
    assert!(
        !raw_lc.contains("\"type\":\"delta\"") && !raw_lc.contains("\"type\": \"delta\""),
        "{what} must not stream assistant deltas: {}",
        out.raw
    );
    assert!(
        !raw_lc.contains("\"role\":\"assistant\"") && !raw_lc.contains("\"role\": \"assistant\""),
        "{what} must not stream assistant text: {}",
        out.raw
    );
    for secret in secrets {
        assert!(
            !out.raw.contains(secret),
            "{what} raw body must not contain stored title or echoed history `{secret}`: {}",
            out.raw
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
        "{what} must not persist a user row with the stream content: {:?}",
        contents
    );
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

/// Plane-disabled grok/ollama and unconfigured openai must 400 before
/// NDJSON / `select a runner` / enqueue. Any 4xx or 200+type=error is
/// too loose: dropping `require_plane_chat_enabled` still 400s later or
/// streams type=error after spawn.
fn assert_disabled_provider_400_before_ndjson(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be HTTP 400 before NDJSON/enqueue, not {} {}",
        out.status,
        out.body
    );
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream type=done: {}",
        out.raw
    );
    if let Some(role) = out.body["message"]["role"].as_str() {
        assert_ne!(
            role, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
}

fn user_hi() -> Value {
    json!([{ "role": "user", "content": "hi" }])
}

/// Owner thread plus one planted user turn so a skipped resume check that
/// loads `list_messages` would echo the secret. HTTP plant only.
struct OwnerConversation {
    a_jar: CookieJar,
    a_ws: String,
    a_uuid: Uuid,
    a_cid: String,
    title: String,
    stored_history: String,
    unique_hi: String,
    snap_count: usize,
}

impl OwnerConversation {
    fn secrets(&self) -> [&str; 3] {
        [
            self.title.as_str(),
            self.unique_hi.as_str(),
            self.stored_history.as_str(),
        ]
    }
}

async fn plant_owner_conversation(app: &axum::Router, state: &AppState) -> OwnerConversation {
    let tag = Uuid::new_v4();
    let title = format!("unique-secret-{tag}");
    let unique_hi = format!("unique-hi-{tag}");
    let stored_history = format!("stored-history-{tag}");

    let (_a_email, a_jar, a_ws) = signup_and_login(app).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");

    let created = call(
        app,
        "POST",
        &conversations_path(&a_ws),
        Some(&a_jar),
        Some(json!({ "title": title })),
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
        app,
        "GET",
        &conversation_path(&a_ws, &a_cid),
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
        Some(title.as_str()),
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

    OwnerConversation {
        a_jar,
        a_ws,
        a_uuid,
        a_cid,
        title,
        stored_history,
        unique_hi,
        snap_count,
    }
}

async fn invite_existing_member(
    app: &axum::Router,
    owner_jar: &CookieJar,
    ws: &str,
) -> CookieJar {
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

/// Complete / GET / DELETE resume IDOR: 404 conversation before plane-disabled
/// 400. Not membership 403 (caller is a member). Must not echo planted secrets.
fn assert_http_conversation_not_found(out: &HttpOut, secrets: &[&str], what: &str) {
    assert_ne!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} must be 404 conversation (caller is a member), not 403 membership: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "{what} must be 404 conversation before plane-disabled 400: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("conversation"),
        "{what} 404 must name conversation: {}",
        out.body
    );
    for secret in secrets {
        assert!(
            !out.raw.contains(secret),
            "{what} raw body must not contain stored title or echoed history `{secret}`: {}",
            out.raw
        );
    }
}

/// Same user POST /workspaces/{B}/chat/stream with A's conversation_id must
/// 404 (or 200 type=error only) before plane-disabled 400. Teammate stream
/// is `chat_stream_teammate_conversation_user_scope`. Filter `-- stream_conversation`.
#[tokio::test]
async fn chat_stream_conversation_user_scope() {
    let (app, state) = app_with_state().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let secrets = planted.secrets();

    // Pair a runner on B so skipping ensure_conversation would enqueue
    // chat_turn instead of 400 "select a runner".
    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&planted.a_jar),
        Some(json!({ "name": format!("stream-scope-b-{}", Uuid::new_v4()) })),
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
    assert_ne!(planted.a_ws, b_ws, "A and B must be distinct workspaces");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");
    let b_runner = create_paired_online_runner(&app, &planted.a_jar, &b_ws).await;

    let other_ws = post_stream_timed(
        &app,
        &planted.a_jar,
        &b_ws,
        json!({
            "messages": [{ "role": "user", "content": planted.unique_hi }],
            "provider": "grok",
            "conversation_id": planted.a_cid,
            "runner_id": b_runner,
        }),
    )
    .await;
    assert_stream_conversation_fail_closed(
        &other_ws,
        &secrets,
        "same user POST /workspaces/{B}/chat/stream conversation_id=A",
    );
    let after_b = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&planted.a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after_b,
        &planted.a_cid,
        &planted.title,
        planted.snap_count,
        &planted.stored_history,
        &planted.unique_hi,
        "after B stream",
    );
    assert_no_chat_turn(&state, planted.a_uuid, "B stream vs A's workspace").await;
    assert_no_chat_turn(&state, b_uuid, "B stream vs B's workspace").await;
}

/// Invited teammate POST /workspaces/{A}/chat/stream with the owner's
/// conversation_id must 404 (or 200 type=error only) before plane-disabled 400.
#[tokio::test]
async fn chat_stream_teammate_conversation_user_scope() {
    let (app, state) = app_with_state().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let secrets = planted.secrets();
    let member_jar = invite_existing_member(&app, &planted.a_jar, &planted.a_ws).await;
    let a_runner = create_paired_online_runner(&app, &planted.a_jar, &planted.a_ws).await;

    let teammate = post_stream_timed(
        &app,
        &member_jar,
        &planted.a_ws,
        json!({
            "messages": [{ "role": "user", "content": planted.unique_hi }],
            "provider": "ollama",
            "conversation_id": planted.a_cid,
            "runner_id": a_runner,
        }),
    )
    .await;
    assert_stream_conversation_fail_closed(
        &teammate,
        &secrets,
        "teammate POST /workspaces/{A}/chat/stream conversation_id=owner",
    );
    let after = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&planted.a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after,
        &planted.a_cid,
        &planted.title,
        planted.snap_count,
        &planted.stored_history,
        &planted.unique_hi,
        "after teammate stream",
    );
    assert_no_chat_turn(&state, planted.a_uuid, "teammate stream vs A's workspace").await;
}

/// Invited teammate POST /chat with the owner's conversation_id is 404
/// conversation before plane-disabled 400. Stream resume is the stream
/// user-scope tests.
#[tokio::test]
async fn chat_complete_conversation_user_scope() {
    let (app, state) = app_with_state().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let secrets = planted.secrets();
    let member_jar = invite_existing_member(&app, &planted.a_jar, &planted.a_ws).await;
    let a_runner = create_paired_online_runner(&app, &planted.a_jar, &planted.a_ws).await;

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&planted.a_ws),
        &member_jar,
        Some(json!({
            "messages": [{ "role": "user", "content": planted.unique_hi }],
            "provider": "grok",
            "conversation_id": planted.a_cid,
            "runner_id": a_runner,
        })),
    )
    .await;
    assert_http_conversation_not_found(
        &complete,
        &secrets,
        "teammate POST /workspaces/{A}/chat conversation_id=owner",
    );
    let after = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&planted.a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after,
        &planted.a_cid,
        &planted.title,
        planted.snap_count,
        &planted.stored_history,
        &planted.unique_hi,
        "after teammate complete",
    );
    assert_no_chat_turn(&state, planted.a_uuid, "teammate complete vs A's workspace").await;
}

/// Invited member GET /conversations/{owner_cid} is 404 and must not
/// return the owner's title/messages. Non-member 403||404 is
/// `chat_status_create_list_delete_other_user_forbidden`.
#[tokio::test]
async fn get_conversation_http_teammate_is_404() {
    let (app, state) = app_with_state().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let secrets = planted.secrets();
    let member_jar = invite_existing_member(&app, &planted.a_jar, &planted.a_ws).await;

    let got = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&member_jar),
        None,
    )
    .await;
    assert_http_conversation_not_found(
        &got,
        &secrets,
        "teammate GET /workspaces/{A}/chat/conversations/{owner_cid}",
    );
}

/// Invited member DELETE /conversations/{owner_cid} is 404 and the
/// owner's row + planted history remain.
#[tokio::test]
async fn delete_conversation_http_teammate_is_404_row_remains() {
    let (app, state) = app_with_state().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let secrets = planted.secrets();
    let member_jar = invite_existing_member(&app, &planted.a_jar, &planted.a_ws).await;

    let deleted = call(
        &app,
        "DELETE",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&member_jar),
        None,
    )
    .await;
    assert_http_conversation_not_found(
        &deleted,
        &secrets,
        "teammate DELETE /workspaces/{A}/chat/conversations/{owner_cid}",
    );
    assert_ne!(
        deleted.body.get("deleted"),
        Some(&json!(true)),
        "teammate DELETE must not report deleted=true: {}",
        deleted.body
    );
    let leftover = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&planted.a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &leftover,
        &planted.a_cid,
        &planted.title,
        planted.snap_count,
        &planted.stored_history,
        &planted.unique_hi,
        "owner GET after teammate DELETE",
    );
}

/// Hard-deleted owner thread (DELETE 200 deleted=true) for resume leftover.
struct DeletedConversation {
    jar: CookieJar,
    ws: String,
    ws_uuid: Uuid,
    user_id: Uuid,
    cid: String,
    cid_uuid: Uuid,
    title: String,
    unique_hi: String,
}

impl DeletedConversation {
    fn secrets(&self) -> [&str; 2] {
        [self.title.as_str(), self.unique_hi.as_str()]
    }
}

async fn plant_deleted_conversation(app: &axum::Router) -> DeletedConversation {
    let tag = Uuid::new_v4();
    let title = format!("unique-secret-{tag}");
    let unique_hi = format!("unique-hi-{tag}");

    let (_email, jar, ws) = signup_and_login(app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let me = call(app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "me: {}", me.body);
    let user_id = Uuid::parse_str(
        me.body["user"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("user.id: {}", me.body)),
    )
    .expect("user id uuid");

    let created = call(
        app,
        "POST",
        &conversations_path(&ws),
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
        app,
        "DELETE",
        &conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body["deleted"], true);

    DeletedConversation {
        jar,
        ws,
        ws_uuid,
        user_id,
        cid,
        cid_uuid,
        title,
        unique_hi,
    }
}

/// DELETE is a hard delete. POST /chat/stream with that `conversation_id`
/// must 404 or emit only `type=error` (no `type=done`, no assistant text),
/// must not enqueue a `chat_turn`, and must not recreate the row.
/// Complete resume is `chat_complete_deleted_conversation_stays_gone`.
/// GET after DELETE is `get_conversation_http_after_delete_is_404`.
#[tokio::test]
async fn chat_stream_deleted_conversation_stays_gone() {
    let (app, state) = app_with_state().await;
    let planted = plant_deleted_conversation(&app).await;
    let secrets = planted.secrets();
    // Pair a runner so skipping require_conversation_if_resuming would
    // enqueue chat_turn instead of 400 "select a runner".
    let runner = create_paired_online_runner(&app, &planted.jar, &planted.ws).await;

    let streamed = post_stream_timed(
        &app,
        &planted.jar,
        &planted.ws,
        json!({
            "messages": [{ "role": "user", "content": planted.unique_hi }],
            "provider": "grok",
            "conversation_id": planted.cid,
            "runner_id": runner,
        }),
    )
    .await;
    assert_stream_conversation_fail_closed(
        &streamed,
        &secrets,
        "POST /chat/stream grok conversation_id=deleted",
    );

    let after = call(
        &app,
        "GET",
        &conversation_path(&planted.ws, &planted.cid),
        Some(&planted.jar),
        None,
    )
    .await;
    assert_http_conversation_not_found(
        &after,
        &secrets,
        "GET /conversations/{cid} after stream vs deleted",
    );
    assert_deleted_conversation_not_in_db(
        &state,
        planted.cid_uuid,
        planted.ws_uuid,
        planted.user_id,
        "after stream",
    )
    .await;
    assert_no_chat_turn(&state, planted.ws_uuid, "stream vs deleted conversation").await;
}

/// Owner POST /chat with a deleted conversation_id is 404 conversation
/// before plane-disabled 400. Leftover GET stays 404; no chat_turn, no row.
#[tokio::test]
async fn chat_complete_deleted_conversation_stays_gone() {
    let (app, state) = app_with_state().await;
    let planted = plant_deleted_conversation(&app).await;
    let secrets = planted.secrets();
    let runner = create_paired_online_runner(&app, &planted.jar, &planted.ws).await;

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&planted.ws),
        &planted.jar,
        Some(json!({
            "messages": [{ "role": "user", "content": planted.unique_hi }],
            "provider": "grok",
            "conversation_id": planted.cid,
            "runner_id": runner,
        })),
    )
    .await;
    assert_http_conversation_not_found(
        &complete,
        &secrets,
        "POST /chat grok conversation_id=deleted",
    );
    let after = call(
        &app,
        "GET",
        &conversation_path(&planted.ws, &planted.cid),
        Some(&planted.jar),
        None,
    )
    .await;
    assert_http_conversation_not_found(
        &after,
        &secrets,
        "GET /conversations/{cid} after complete vs deleted",
    );
    assert_deleted_conversation_not_in_db(
        &state,
        planted.cid_uuid,
        planted.ws_uuid,
        planted.user_id,
        "after complete",
    )
    .await;
    assert_no_chat_turn(&state, planted.ws_uuid, "complete vs deleted conversation").await;
}

/// Owner GET /conversations/{cid} immediately after DELETE is 404
/// conversation and list omits cid. Stream leftover GET is the stream test.
#[tokio::test]
async fn get_conversation_http_after_delete_is_404() {
    let (app, _state) = app_with_state().await;
    let planted = plant_deleted_conversation(&app).await;
    let secrets = planted.secrets();

    let leftover = call(
        &app,
        "GET",
        &conversation_path(&planted.ws, &planted.cid),
        Some(&planted.jar),
        None,
    )
    .await;
    assert_http_conversation_not_found(
        &leftover,
        &secrets,
        "GET /conversations/{cid} immediately after DELETE",
    );
    let listed = call(
        &app,
        "GET",
        &conversations_path(&planted.ws),
        Some(&planted.jar),
        None,
    )
    .await;
    assert_deleted_conversation_absent_from_list(
        &listed,
        &planted.cid,
        &secrets,
        "list after DELETE",
    );
}

/// POST /chat and /chat/stream reject role=tool with HTTP 400 (stream must
/// not 200 NDJSON). Must not INSERT a conversation or enqueue chat_turn.
/// Four-name matrix is prepare_chat_pairs_rejects_tool_function_admin_root.
/// Resume append is chat_invalid_role_resume_does_not_append. Filter
/// `-- chat_invalid_role`.
#[tokio::test]
async fn chat_invalid_role_stream_rejected_no_persist() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);
    let secret = format!("invalid-role-tool-{}", Uuid::new_v4());
    let body = json!({
        "messages": [
            { "role": "tool", "content": secret },
            { "role": "user", "content": "hi" }
        ],
        "provider": "ollama",
        "runner_id": runner_id,
    });

    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_role_rejected_http(&complete, "POST /chat role=tool");
    let streamed = post_stream_timed(&app, &jar, &ws, body).await;
    assert_chat_role_rejected_stream_http(&streamed, "POST /chat/stream role=tool");

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after role=tool: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after role=tool reject: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "role=tool stream").await;
}

/// Resume POST /chat/stream with conversation_id + role=tool must 400 and
/// leave GET /conversations/{id} message count/contents unchanged.
#[tokio::test]
async fn chat_invalid_role_resume_does_not_append() {
    let (app, state) = app_with_plane_chat().await;
    let planted = plant_owner_conversation(&app, &state).await;
    let runner_id = create_paired_online_runner(&app, &planted.a_jar, &planted.a_ws).await;
    let secret = format!("resume-tool-{}", Uuid::new_v4());

    let streamed = post_stream_timed(
        &app,
        &planted.a_jar,
        &planted.a_ws,
        json!({
            "messages": [
                { "role": "tool", "content": secret },
                { "role": "user", "content": planted.unique_hi }
            ],
            "provider": "ollama",
            "runner_id": runner_id,
            "conversation_id": planted.a_cid,
        }),
    )
    .await;
    assert_chat_role_rejected_stream_http(
        &streamed,
        "POST /chat/stream resume role=tool",
    );

    let got = call(
        &app,
        "GET",
        &conversation_path(&planted.a_ws, &planted.a_cid),
        Some(&planted.a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &got,
        &planted.a_cid,
        &planted.title,
        planted.snap_count,
        &planted.stored_history,
        &planted.unique_hi,
        "GET /conversations/{id} after resume role=tool",
    );
    assert_no_chat_turn(&state, planted.a_uuid, "resume role=tool").await;
}

/// SUT trim+to_lowercase: role=" USER " / "SYSTEM" + user hi must not 400.
#[tokio::test]
async fn chat_stream_uppercase_or_padded_allowed_role_not_invalid() {
    let (app, _state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let streamed = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [
                { "role": " USER ", "content": "hi" },
                { "role": "SYSTEM", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "provider": "ollama",
        }),
    )
    .await;
    assert!(
        !streamed.status.is_server_error(),
        "padded/uppercase allowed roles must not 5xx: {} {}",
        streamed.status,
        streamed.body
    );
    assert_ne!(
        streamed.status,
        StatusCode::BAD_REQUEST,
        "role=\" USER \" / \"SYSTEM\" + user hi must not 400: {} {}",
        streamed.status,
        streamed.body
    );
}

fn listed_conversation_count(body: &Value) -> usize {
    body["conversations"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("conversations array: {body}"))
}

fn assert_chat_role_rejected_http(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400, not a completed turn: {} {}",
        out.status,
        out.body
    );
}

fn assert_chat_role_rejected_stream_http(out: &HttpOut, what: &str) {
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{what} must be 400, not 200 NDJSON ({ctype}): {}",
        out.raw
    );
    assert_chat_role_rejected_http(out, what);
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream type=done: {}",
        out.raw
    );
}

/// POST /chat/stream rejects a 32001-byte user line with HTTP 400
/// (not 200 NDJSON) and persists nothing. Complete is
/// `chat_message_max_32k_complete_rejected_no_persist`. At-cap is
/// `chat_message_max_32000_bytes_is_not_too_long`. Filter `-- chat_message_max`.
#[tokio::test]
async fn chat_message_max_32k_stream_rejected_no_persist() {
    // Plane-enabled so stream reaches prepare_chat_pairs. Disabled ollama
    // 400s at require_plane_chat_enabled and would make every stream 400
    // a tautology that misses a dropped 32k gate.
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    let streamed = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": huge }],
            "provider": "ollama",
            "runner_id": runner_id,
        }),
    )
    .await;
    assert_message_too_long_stream_http(&streamed, &huge, "POST /chat/stream");

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after 32001-byte stream: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after 32001-byte stream reject: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "32001-byte user stream").await;
}

/// POST /chat rejects a 32001-byte user line with HTTP 400 and persists
/// nothing. Stream is `chat_message_max_32k_stream_rejected_no_persist`.
#[tokio::test]
async fn chat_message_max_32k_complete_rejected_no_persist() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &jar,
        Some(json!({
            "messages": [{ "role": "user", "content": huge }],
            "provider": "ollama",
            "runner_id": runner_id,
        })),
    )
    .await;
    assert_message_too_long_http(&complete, &huge, "POST /chat");

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after 32001-byte complete: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after 32001-byte complete reject: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "32001-byte user complete").await;
}

/// `content.len() > 32_000` — 32_000 ASCII bytes is not too-long.
/// Trim/UTF-8 is `chat_message_max_is_bytes_after_trim`.
#[tokio::test]
async fn chat_message_max_32000_bytes_is_not_too_long() {
    let (app, _state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let at_cap = "y".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);

    let streamed = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": at_cap }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_message_too_long_http(&streamed, "32_000-byte POST /chat/stream");
}

/// Allowed non-user roles are still length-gated. Short user + 32001-byte
/// assistant|system must 400 and persist nothing.
#[tokio::test]
async fn chat_message_max_rejects_overlong_assistant_or_system() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    for role in ["assistant", "system"] {
        let streamed = post_stream_timed(
            &app,
            &jar,
            &ws,
            json!({
                "messages": [
                    { "role": role, "content": huge },
                    { "role": "user", "content": "hi" }
                ],
                "provider": "ollama",
                "runner_id": runner_id,
            }),
        )
        .await;
        assert_message_too_long_stream_http(
            &streamed,
            &huge,
            &format!("POST /chat/stream role={role} 32001-byte"),
        );
    }

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after overlong assistant|system: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after overlong assistant|system: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "overlong assistant|system").await;
}

/// SUT checks every pair after trim — a later 32001-byte user line is 400.
#[tokio::test]
async fn chat_message_max_rejects_later_user_line() {
    let (app, _state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    let streamed = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "user", "content": huge }
            ],
            "provider": "ollama",
            "runner_id": runner_id,
        }),
    )
    .await;
    assert_message_too_long_stream_http(
        &streamed,
        &huge,
        "POST /chat/stream later 32001-byte user line",
    );
}

/// Gate is trim then `content.len()` (bytes). 32000 ASCII + trailing spaces
/// is not too-long; 8001 × U+1F600 (32004 bytes) is 400.
#[tokio::test]
async fn chat_message_max_is_bytes_after_trim() {
    let (app, _state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;

    let padded = format!("{}   ", "z".repeat(32_000));
    assert_eq!(padded.trim().len(), 32_000);
    assert!(padded.len() > 32_000);
    let padded_stream = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": padded }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_message_too_long_http(
        &padded_stream,
        "32000 ASCII + trailing spaces POST /chat/stream",
    );

    let emoji = "\u{1F600}".repeat(8001);
    assert_eq!(emoji.len(), 32_004);
    let emoji_stream = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": emoji }],
            "provider": "ollama",
            "runner_id": runner_id,
        }),
    )
    .await;
    assert_message_too_long_stream_http(
        &emoji_stream,
        &emoji,
        "POST /chat/stream 8001×U+1F600 (32004 bytes)",
    );
}

fn assert_message_too_long_http(out: &HttpOut, huge: &str, what: &str) {
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
    assert_eq!(
        out.body["error"].as_str(),
        Some("message too long (max 32k bytes)"),
        "{what} 400 error must be the 32k-byte gate: {}",
        out.body
    );
    assert!(
        !out.raw.contains(huge),
        "{what} 400 body must not echo the overlong line: {}",
        out.status
    );
}

fn assert_message_too_long_stream_http(out: &HttpOut, huge: &str, what: &str) {
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
    assert_message_too_long_http(out, huge, what);
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.status
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream type=done: {}",
        out.status
    );
}

fn assert_not_message_too_long_http(out: &HttpOut, what: &str) {
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
    assert_ne!(
        out.body["error"].as_str(),
        Some("message too long (max 32k bytes)"),
        "{what} must not 400 as too long (400 from no provider/model/runner is ok): {}",
        out.body
    );
}

/// Plane grok/ollama off and OpenAI unconfigured: POST /chat/stream must
/// HTTP 400 before NDJSON / enqueue. Omitted provider defaults to ollama.
/// Claude is not plane-gated; unknown parse_provider is providers.rs.
/// Complete is `chat_complete_disabled_grok_ollama_openai_is_400_no_enqueue`.
/// runner_id is `chat_stream_disabled_providers_with_runner_id_still_400_no_enqueue`.
#[tokio::test]
async fn chat_stream_disabled_providers_fail_closed_quickly() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);

    for provider in ["grok", "ollama", "openai"] {
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
        assert_disabled_provider_400_before_ndjson(
            &out,
            &format!("POST /chat/stream provider={provider}"),
        );
    }

    let defaulted = post_stream_timed(&app, &jar, &ws, json!({ "messages": user_hi() })).await;
    assert_disabled_provider_400_before_ndjson(
        &defaulted,
        "POST /chat/stream omitted provider (default ollama)",
    );

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after disabled stream: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "disabled stream must not INSERT a conversation: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "disabled grok/ollama/openai/default stream").await;
}

/// Same all-disabled fixture: POST /chat (not stream) grok|ollama|openai
/// and omitted provider is 400, no chat_turn, no conversation row.
#[tokio::test]
async fn chat_complete_disabled_grok_ollama_openai_is_400_no_enqueue() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);

    for provider in ["grok", "ollama", "openai"] {
        let out = call_timed(
            &app,
            "POST",
            &chat_path(&ws),
            &jar,
            Some(json!({
                "messages": user_hi(),
                "provider": provider
            })),
        )
        .await;
        assert_disabled_provider_400_before_ndjson(
            &out,
            &format!("POST /chat provider={provider}"),
        );
    }

    let defaulted = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &jar,
        Some(json!({ "messages": user_hi() })),
    )
    .await;
    assert_disabled_provider_400_before_ndjson(
        &defaulted,
        "POST /chat omitted provider (default ollama)",
    );

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after disabled complete: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "disabled complete must not INSERT a conversation: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "disabled grok/ollama/openai/default complete").await;
}

/// Pair an online runner, then POST /chat/stream grok|ollama|openai +
/// runner_id is still 400 before NDJSON / enqueue. Without runner_id,
/// dropping the plane gate would 200 type=error or wait 240s.
#[tokio::test]
async fn chat_stream_disabled_providers_with_runner_id_still_400_no_enqueue() {
    let (app, state) = app_with_state().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let conversations = conversations_path(&ws);

    let snap = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);

    for provider in ["grok", "ollama", "openai"] {
        let out = post_stream_timed(
            &app,
            &jar,
            &ws,
            json!({
                "messages": user_hi(),
                "provider": provider,
                "runner_id": runner_id,
            }),
        )
        .await;
        assert_disabled_provider_400_before_ndjson(
            &out,
            &format!("POST /chat/stream {provider}+runner_id"),
        );
    }

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after disabled stream+runner_id: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "disabled stream+runner_id must not INSERT a conversation: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, ws_uuid, "disabled grok/ollama/openai+runner_id stream").await;
}

/// POST /chat and /chat/stream reject empty / typo / ticket provider as HTTP
/// 400 (not a turn, not default ollama). Unknown parse_provider is this
/// lock; plane-disabled grok/ollama/openai is the stream sibling above.
#[tokio::test]
async fn chat_complete_and_stream_empty_typo_ticket_provider_is_400() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");

    for provider in ["", " ", "grokk", "github"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": runner_id,
        });
        let complete =
            call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_eq!(
            complete.status,
            StatusCode::BAD_REQUEST,
            "POST /chat provider={provider:?} must be 400 (not a turn, not default ollama): {} {}",
            complete.status,
            complete.body
        );
        assert!(
            !body_has_ok_true(&complete.raw),
            "POST /chat provider={provider:?} must not complete a turn: {}",
            complete.raw
        );

        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_eq!(
            stream.status,
            StatusCode::BAD_REQUEST,
            "POST /chat/stream provider={provider:?} must be HTTP 400, not 4xx-or-NDJSON: {} {}",
            stream.status,
            stream.body
        );
        assert!(
            !body_has_ok_true(&stream.raw),
            "POST /chat/stream provider={provider:?} must not complete a turn: {}",
            stream.raw
        );
        assert!(
            !stream.raw.contains("\"type\":\"done\"")
                && !stream.raw.contains("\"type\": \"done\""),
            "POST /chat/stream provider={provider:?} must not stream a completed turn: {}",
            stream.raw
        );
    }

    assert_no_chat_turn(&state, ws_uuid, "empty/typo/ticket provider").await;
}

/// POST /chat provider=anthropic|xai must not 400 as unknown provider.
/// Plane-disabled / runner-required is ok.
#[tokio::test]
async fn chat_complete_anthropic_and_xai_are_canonical_not_unknown() {
    let app = app().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    for provider in ["anthropic", "xai"] {
        let out = call_timed(
            &app,
            "POST",
            &chat_path(&ws),
            &jar,
            Some(json!({
                "messages": user_hi(),
                "provider": provider,
            })),
        )
        .await;
        let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
        assert!(
            !err.contains("unknown provider"),
            "POST /chat provider={provider} must not 400 as unknown provider \
             (plane-disabled / runner-required is ok): {} {}",
            out.status,
            out.body
        );
    }
}

/// Other-ws cookie on GET /chat/status is 403 membership before inventory.
/// `?runner_id` is `chat_status_runner_id_other_workspace_cookie_forbidden`.
/// POST /chat enqueue is `chat_status_complete_stream_other_workspace_cookie_forbidden_before_enqueue`.
#[tokio::test]
async fn chat_status_other_workspace_cookie_forbidden() {
    let (app, state) = app_with_state().await;
    let (_a_email, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let status = call_timed(&app, "GET", &status_path(&a_ws), &b_jar, None).await;
    assert_membership_forbidden(&status, "GET /chat/status");

    assert_no_chat_turn(&state, a_uuid, "other-ws cookie GET /chat/status vs owner").await;
    assert_no_chat_turn(&state, b_uuid, "other-ws cookie GET /chat/status vs caller").await;
}

/// Other-ws cookie on GET /chat/status?runner_id is 403 before load_visible_host.
#[tokio::test]
async fn chat_status_runner_id_other_workspace_cookie_forbidden() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let runner_id = create_paired_online_runner(&app, &a_jar, &a_ws).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let status_runner = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={runner_id}", status_path(&a_ws)),
        &b_jar,
        None,
    )
    .await;
    assert_membership_forbidden(&status_runner, "GET /chat/status?runner_id");

    assert_no_chat_turn(
        &state,
        a_uuid,
        "other-ws cookie GET /chat/status?runner_id vs owner",
    )
    .await;
    assert_no_chat_turn(
        &state,
        b_uuid,
        "other-ws cookie GET /chat/status?runner_id vs caller",
    )
    .await;
}

/// Other-ws cookie on POST /chat (plane grok + owner's runner) is 403
/// membership before enqueue / the 240s runner wait.
#[tokio::test]
async fn chat_status_complete_stream_other_workspace_cookie_forbidden_before_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let runner_id = create_paired_online_runner(&app, &a_jar, &a_ws).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&a_ws),
        &b_jar,
        Some(json!({
            "messages": user_hi(),
            "provider": "grok",
            "runner_id": runner_id,
        })),
    )
    .await;
    assert_membership_forbidden(&complete, "POST /chat grok+runner_id");

    assert_no_chat_turn(&state, a_uuid, "other-ws cookie vs owner workspace").await;
    assert_no_chat_turn(&state, b_uuid, "other-ws cookie vs caller workspace").await;
}

/// No cookie on GET /chat/status, POST /chat, POST /chat/stream is 401
/// (require_user), not 403 membership and not a hang.
#[tokio::test]
async fn chat_status_complete_stream_unauthenticated_is_401() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let empty = CookieJar::default();
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": runner_id,
    });

    let status = call_timed(&app, "GET", &status_path(&ws), &empty, None).await;
    assert_unauthenticated(&status, "GET /chat/status");

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &empty,
        Some(body.clone()),
    )
    .await;
    assert_unauthenticated(&complete, "POST /chat");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &empty, Some(body)).await;
    assert_unauthenticated(&stream, "POST /chat/stream");

    assert_no_chat_turn(&state, ws_uuid, "unauthenticated chat").await;
}

/// Other-ws cookie GET/POST /chat/conversations is 403 and must not
/// insert a conversation row (list is user-scoped; count the workspace).
#[tokio::test]
async fn chat_conversations_other_workspace_cookie_forbidden_no_insert() {
    let (app, state) = app_with_state().await;
    let (_a_email, _a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");

    let listed = call_timed(&app, "GET", &conversations_path(&a_ws), &b_jar, None).await;
    assert_membership_forbidden(&listed, "GET /chat/conversations");

    let created = call_timed(
        &app,
        "POST",
        &conversations_path(&a_ws),
        &b_jar,
        Some(json!({ "title": format!("other-ws-{}", Uuid::new_v4()) })),
    )
    .await;
    assert_membership_forbidden(&created, "POST /chat/conversations");

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_conversations WHERE workspace_id = $1",
    )
    .bind(a_uuid)
    .fetch_one(&state.db)
    .await
    .expect("count conversations");
    assert_eq!(
        count, 0,
        "other-ws cookie must not INSERT chat_conversations on owner workspace"
    );
}

/// Invited teammate GET /chat/status and POST /chat is not 403
/// (require_member, not owner-only). Plane off so complete cannot wait 240s.
#[tokio::test]
async fn chat_invited_member_status_and_complete_allowed() {
    let app = app().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let member_jar = invite_existing_member(&app, &a_jar, &a_ws).await;

    let status = call_timed(&app, "GET", &status_path(&a_ws), &member_jar, None).await;
    assert_ne!(
        status.status,
        StatusCode::FORBIDDEN,
        "invited member GET /chat/status must not be 403: {} {}",
        status.status,
        status.body
    );
    assert_eq!(
        status.status,
        StatusCode::OK,
        "invited member GET /chat/status: {} {}",
        status.status,
        status.body
    );

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&a_ws),
        &member_jar,
        Some(json!({
            "messages": user_hi(),
            "provider": "grok",
        })),
    )
    .await;
    assert_ne!(
        complete.status,
        StatusCode::FORBIDDEN,
        "invited member POST /chat must not be 403 (plane-disabled 400 is ok): {} {}",
        complete.status,
        complete.body
    );
}

/// Member cookie on a foreign (non-cluster) `runner_id` is 404, not 403
/// membership, and must not leak that host's heartbeat name/models.
/// Other-ws cookie is the 403 sibling. Cluster visibility is
/// `chat_cluster_runner_id_status_on_other_workspace_is_400`.
/// Filter `-- foreign_runner`.
fn assert_foreign_runner_404(out: &HttpOut, secrets: &[&str], what: &str) {
    assert_ne!(
        out.status,
        StatusCode::FORBIDDEN,
        "{what} must be 404 (caller is a member of this workspace), \
         not 403 membership (list-runner-chat-membership is other-ws cookie \
         on the owner's path): {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::NOT_FOUND,
        "{what} must be 404 — a guessed host UUID from another workspace \
         must not run CLI chat or return that box's inventory: {} {}",
        out.status,
        out.body
    );
    assert!(
        !body_has_ok_true(&out.raw),
        "{what} must not include ok:true as a completed CLI turn: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream a completed turn: {}",
        out.raw
    );
    if let Some(role) = out.body["message"]["role"].as_str() {
        assert_ne!(
            role, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
    let hay = format!("{} {}", out.body, out.raw);
    for secret in secrets {
        assert!(
            !hay.contains(secret),
            "{what} must not return B's agents/models/hostname `{secret}`: {}",
            out.body
        );
    }
    if let Some(providers) = out.body["providers"].as_array() {
        assert!(
            providers.is_empty(),
            "{what} 404 must not advertise B's provider inventory: {}",
            out.body
        );
    }
    if let Some(models) = out.body["models"].as_array() {
        assert!(
            models.is_empty(),
            "{what} 404 must not advertise B's models: {}",
            out.body
        );
    }
}

/// A's cookie POST /workspaces/{A}/chat grok + B's non-cluster host is 404
/// (`load_visible_host` → not_found). Must not leak B's heartbeat name/models,
/// enqueue `chat_turn`, or INSERT a conversation (`ensure_conversation` is
/// before `require_taking`). Other-ws cookie is the 403 sibling. Cluster
/// is `chat_cluster_runner_id_status_on_other_workspace_is_400`. Same-user
/// other-ws is `chat_same_user_other_workspace_runner_id_is_404`.
#[tokio::test]
async fn chat_foreign_runner_id_is_404_and_does_not_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let tag = Uuid::new_v4();
    let b_name = format!("foreign-b-{tag}");
    let b_grok = format!("foreign-b-grok-{tag}");
    let b_ollama = format!("foreign-b-ollama-{tag}");
    let secrets = [b_name.as_str(), b_grok.as_str(), b_ollama.as_str()];

    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let b_host = pair_and_heartbeat(
        &app,
        &b_jar,
        &b_ws,
        &b_name,
        Some(planted_ready_agents(&b_grok, &b_ollama)),
    )
    .await;

    let conversations = conversations_path(&a_ws);
    let snap = call(&app, "GET", &conversations, Some(&a_jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /conversations: {}",
        snap.body
    );
    let snap_count = listed_conversation_count(&snap.body);

    let complete = call_timed(
        &app,
        "POST",
        &chat_path(&a_ws),
        &a_jar,
        Some(json!({
            "messages": user_hi(),
            "provider": "grok",
            "runner_id": b_host,
        })),
    )
    .await;
    assert_foreign_runner_404(
        &complete,
        &secrets,
        "POST /workspaces/{A}/chat grok+B runner_id",
    );

    let after = call(&app, "GET", &conversations, Some(&a_jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after foreign runner_id: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "foreign runner_id complete must not INSERT a conversation: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    assert_no_chat_turn(&state, a_uuid, "A after foreign runner_id").await;
    assert_no_chat_turn(&state, b_uuid, "B after foreign runner_id").await;
}

/// Same cookie creates workspace B and pairs a host there; GET A's
/// `/chat/status?runner_id={B}` is 404 (not "any host this user owns").
/// Two-user POST is `chat_foreign_runner_id_is_404_and_does_not_enqueue`.
#[tokio::test]
async fn chat_same_user_other_workspace_runner_id_is_404() {
    let (app, state) = app_with_state().await;
    let tag = Uuid::new_v4();
    let b_name = format!("same-user-b-{tag}");
    let b_grok = format!("same-user-b-grok-{tag}");
    let b_ollama = format!("same-user-b-ollama-{tag}");
    let secrets = [b_name.as_str(), b_grok.as_str(), b_ollama.as_str()];

    let (_email, jar, a_ws) = signup_and_login(&app).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");

    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("same-user-b-ws-{tag}") })),
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

    let b_host = pair_and_heartbeat(
        &app,
        &jar,
        &b_ws,
        &b_name,
        Some(planted_ready_agents(&b_grok, &b_ollama)),
    )
    .await;

    let status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={b_host}", status_path(&a_ws)),
        &jar,
        None,
    )
    .await;
    assert_foreign_runner_404(
        &status,
        &secrets,
        "same-user GET /workspaces/{A}/chat/status?runner_id={B}",
    );

    assert_no_chat_turn(&state, a_uuid, "same-user foreign runner_id vs A").await;
    assert_no_chat_turn(&state, b_uuid, "same-user foreign runner_id vs B").await;
}

/// Cluster-labeled host is not Run on a PC. GET /chat/status?runner_id= on
/// another workspace is 400. Non-cluster foreign id stays 404.
#[tokio::test]
async fn chat_cluster_runner_id_status_on_other_workspace_is_400() {
    let (app, state) = app_with_state().await;
    let tag = Uuid::new_v4();
    let name = format!("cluster-chat-{tag}");
    let grok = format!("cluster-chat-grok-{tag}");
    let ollama = format!("cluster-chat-ollama-{tag}");

    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");

    let host_id = pair_and_heartbeat(
        &app,
        &a_jar,
        &a_ws,
        &name,
        Some(planted_ready_agents(&grok, &ollama)),
    )
    .await;
    sqlx::query("UPDATE worker_hosts SET labels = array['cluster'] WHERE id = $1")
        .bind(host_id)
        .execute(&state.db)
        .await
        .expect("stamp cluster");
    let stored = fleet_db::repos::worker_hosts::get(&state.db, host_id)
        .await
        .expect("get stamped host")
        .unwrap_or_else(|| panic!("stamped host {host_id}"));
    assert!(
        stored.is_cluster_host(),
        "stamp must set cluster: {stored:?}"
    );

    let status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={host_id}", status_path(&b_ws)),
        &b_jar,
        None,
    )
    .await;
    assert_eq!(
        status.status,
        StatusCode::BAD_REQUEST,
        "cluster runner_id on other-ws GET /chat/status must be 400: {} {}",
        status.status,
        status.body
    );
    let hay = format!("{} {}", status.body, status.raw).to_ascii_lowercase();
    assert!(
        hay.contains("not run on a pc") || hay.contains("not a pc"),
        "400 body must say this runner is not Run on a PC: {}",
        status.body
    );
    let _ = (grok, ollama, name);
}

/// Empty / whitespace-only messages on POST /chat and /chat/stream must 400
/// before `run_on_runner` enqueues a `chat_turn` (grok|ollama + runner_id).
#[tokio::test]
async fn chat_complete_and_stream_empty_messages_rejected_before_runner_wait() {
    // Plane on so stream whitespace reaches prepare_chat_pairs, not
    // require_plane_chat_enabled. `[]` is rejected before the plane check
    // on both routes; complete whitespace is rejected while building pairs.
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");

    let cases = [
        json!({ "messages": [], "provider": "grok", "runner_id": runner_id }),
        json!({ "messages": [], "provider": "ollama", "runner_id": runner_id }),
        json!({
            "messages": [{ "role": "user", "content": "   " }],
            "provider": "grok",
            "runner_id": runner_id
        }),
        json!({
            "messages": [{ "role": "user", "content": " \n\t " }],
            "provider": "ollama",
            "runner_id": runner_id
        }),
    ];

    for body in cases {
        let label = body.to_string();
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_empty_bad_request(&complete, &format!("POST /chat {label}"));
        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_empty_bad_request(&stream, &format!("POST /chat/stream {label}"));
    }

    assert_no_chat_turn(&state, ws_uuid, "empty/whitespace messages").await;
}

/// POST /chat with a taking runner and user+assistant messages must enqueue
/// `chat_turn` whose `payload.prompt` is the labeled CLI transcript.
/// Own-runner enqueue controls only lock kind and host_id.
#[tokio::test]
async fn enqueued_chat_turn_prompt_is_labeled_transcript() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let user_text = format!("prompt-user-{tag}");
    let assistant_text = format!("prompt-assistant-{tag}");
    let runner_id = create_paired_online_runner(&app, &jar, &ws).await;
    let expected = fleet_server::runner_chat::format_transcript(&[
        ("user".into(), user_text.clone()),
        ("assistant".into(), assistant_text.clone()),
    ]);

    let live_body = json!({
        "messages": [
            { "role": "user", "content": user_text },
            { "role": "assistant", "content": assistant_text },
        ],
        "provider": "grok",
        "runner_id": runner_id,
    });
    let live_app = app.clone();
    let live_jar = jar.clone();
    let live_path = chat_path(&ws);
    let live_handle = tokio::spawn(async move {
        call(
            &live_app,
            "POST",
            &live_path,
            Some(&live_jar),
            Some(live_body),
        )
        .await
    });
    tokio::pin!(live_handle);

    let poll = async {
        loop {
            if let Some(c) = fleet_db::repos::work_claims::claim_next(
                &state.db,
                ws_uuid,
                Uuid::now_v7(),
                chrono::Duration::minutes(1),
            )
            .await
            .expect("claim_next")
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    tokio::select! {
        claim = poll => {
            live_handle.abort();
            assert_eq!(
                claim.kind, "chat_turn",
                "POST /chat must enqueue chat_turn: {claim:?}"
            );
            let prompt = claim
                .payload
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                prompt.contains(&expected),
                "chat_turn.payload.prompt must include the labeled transcript: {prompt}"
            );
            assert!(
                prompt.contains("System:\n"),
                "runner chat must prepend the system prompt ahead of the transcript: {prompt}"
            );
            assert_eq!(
                claim.payload["max_turns"].as_i64(),
                Some(32),
                "chat_turn.payload.max_turns must be the workspace default: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "POST /chat returned {} {} before a chat_turn was claimed — \
                 taking runner must enqueue, not 400/404",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!("POST /chat did not enqueue chat_turn within {FAIL_CLOSED:?}");
        }
    }
}

fn body_has_ok_true(raw: &str) -> bool {
    raw.contains("\"ok\":true") || raw.contains("\"ok\": true")
}

/// Revoked runner: 400 before enqueue / the 240s wait. Not a fake completed turn.
/// Stream must not 200 a turn either — this gate is before NDJSON starts.
/// Wording is `runner … is removed` (`ensure_online` / host_is_removed).
/// Do not treat plane-disabled (`… chat is disabled`) as this leftover.
fn assert_revoked_runner_rejected(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 (runner is removed), \
         not a hang or fake assistant turn: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("removed"),
        "{what} 400 must name removed (not plane-disabled): {}",
        out.body
    );
    assert!(
        !body_has_ok_true(&out.raw),
        "{what} must not include ok:true as a completed CLI turn: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream a completed turn: {}",
        out.raw
    );
    if let Some(role) = out.body["message"]["role"].as_str() {
        assert_ne!(
            role, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
}

/// DELETE revoke sets status=disabled and drops the flm_. Chat must 400 quickly
/// and must not enqueue chat_turn — `ensure_online` ignoring status=disabled
/// is the hole (fresh last_seen still passes for 90s).
/// Still-online enqueue is `chat_online_runner_enqueues_chat_turn`.
#[tokio::test]
async fn chat_revoked_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();

    let hid = pair_and_heartbeat(&app, &jar, &ws, &format!("revoked-{tag}"), None).await;

    let revoked = call(
        &app,
        "DELETE",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        revoked.status,
        StatusCode::OK,
        "DELETE /worker-hosts/{{hid}} must revoke: {}",
        revoked.body
    );

    // Leftover stays. last_seen is still fresh and enabled is still true —
    // revoke does not PATCH those — so last_seen + enabled would still pass.
    let leftover = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        leftover.status,
        StatusCode::OK,
        "GET revoked leftover: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["status"], "disabled",
        "GET leftover must stay status=disabled (not hard-deleted): {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["enabled"], true,
        "revoke must not be the enabled=false held card: {}",
        leftover.body
    );
    assert!(
        leftover.body["last_seen_at"].is_string(),
        "revoke must keep last_seen (not the pending/offline card): {}",
        leftover.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_revoked_runner_rejected(&complete, "POST /chat grok+revoked");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_revoked_runner_rejected(&stream, "POST /chat/stream grok+revoked");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "revoked runner_id must not enqueue a new chat_turn"
    );
}

/// At-cap runner: POST /chat and /chat/stream are HTTP 400 before enqueue
/// / NDJSON / the 240s wait. `require_taking` runs before the stream starts,
/// so a 200 `type=error` body is not this gate.
fn assert_at_job_cap_chat_rejected(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be HTTP 400 at cap before enqueue/NDJSON, not {} {}",
        out.status,
        out.body
    );
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    assert!(
        !body_has_ok_true(&out.raw),
        "{what} must not include ok:true as a completed CLI turn: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream a completed turn: {}",
        out.raw
    );
    if let Some(role) = out.body["message"]["role"].as_str() {
        assert_ne!(
            role, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
}

async fn chat_turn_count(state: &AppState, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims WHERE workspace_id = $1 AND kind = 'chat_turn'",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await
    .expect("count chat_turn")
}

/// Heartbeat `current_jobs` at effective max must 400 POST /chat and
/// /chat/stream inside FAIL_CLOSED — not enqueue `chat_turn` / wait 240s.
/// `ensure_online` skipping the cap is the hole (host stays online).
/// Under-cap control: same plane max=1 + current_jobs=0 still enqueues.
#[tokio::test]
async fn chat_at_job_cap_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let grok_model = format!("at-cap-grok-{tag}");
    let ollama_model = format!("at-cap-ollama-{tag}");
    let (hid, machine) = pair_and_heartbeat_machine(
        &app,
        &jar,
        &ws,
        &format!("at-cap-{tag}"),
        Some(planted_ready_agents(&grok_model, &ollama_model)),
    )
    .await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let host_path = format!("{}/{hid}", hosts_path(&ws));
    let before = chat_turn_count(&state, ws_uuid).await;

    let patched = call(
        &app,
        "PATCH",
        &host_path,
        Some(&jar),
        Some(json!({ "max_concurrent": 1 })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "PATCH max_concurrent=1: {}",
        patched.body
    );
    assert_eq!(patched.body["max_concurrent"], 1, "{}", patched.body);
    assert_eq!(patched.body["status"], "online", "{}", patched.body);
    assert_eq!(patched.body["paused"], false, "{}", patched.body);
    assert_eq!(patched.body["drain"], false, "{}", patched.body);
    assert_eq!(patched.body["enabled"], true, "{}", patched.body);

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 1,
            "max_concurrent": 8,
            "agents": planted_ready_agents(&grok_model, &ollama_model),
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat current_jobs=1: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["current_jobs"], 1, "{}", hb.body);
    assert_eq!(
        hb.body["host"]["max_concurrent"], 1,
        "plane 1 must win over advertised 8: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["status"], "online", "{}", hb.body);
    assert_eq!(hb.body["host"]["paused"], false, "{}", hb.body);
    assert_eq!(hb.body["host"]["drain"], false, "{}", hb.body);
    assert_eq!(hb.body["host"]["enabled"], true, "{}", hb.body);

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after at-cap heartbeat: {}",
        got.body
    );
    assert_eq!(got.body["current_jobs"], 1, "{}", got.body);
    assert_eq!(got.body["max_concurrent"], 1, "{}", got.body);
    assert_eq!(got.body["status"], "online", "{}", got.body);

    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_at_job_cap_chat_rejected(&complete, "POST /chat grok+at-cap");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_at_job_cap_chat_rejected(&stream, "POST /chat/stream grok+at-cap");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "at-cap runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "at-cap runner_id after current_jobs=1").await;

    // Control: same plane max=1 + current_jobs=0 still enqueues (not a
    // tautology that every runner_id 400s as at-cap).
    let hb_clear = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "max_concurrent": 8,
            "agents": planted_ready_agents(&grok_model, &ollama_model),
        })),
    )
    .await;
    assert_eq!(
        hb_clear.status,
        StatusCode::OK,
        "heartbeat current_jobs=0: {}",
        hb_clear.body
    );
    assert_eq!(hb_clear.body["host"]["current_jobs"], 0, "{}", hb_clear.body);
    assert_eq!(
        hb_clear.body["host"]["max_concurrent"], 1,
        "plane 1 must still win over advertised 8: {}",
        hb_clear.body
    );
    assert_eq!(hb_clear.body["host"]["status"], "online", "{}", hb_clear.body);

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let live_app = app.clone();
    let live_jar = jar.clone();
    let live_path = chat_path(&ws);
    let live_handle = tokio::spawn(async move {
        call(
            &live_app,
            "POST",
            &live_path,
            Some(&live_jar),
            Some(live_body),
        )
        .await
    });
    tokio::pin!(live_handle);

    let poll = async {
        loop {
            if let Some(c) = fleet_db::repos::work_claims::claim_next(
                &state.db,
                ws_uuid,
                Uuid::now_v7(),
                chrono::Duration::minutes(1),
            )
            .await
            .expect("claim_next")
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    tokio::select! {
        claim = poll => {
            live_handle.abort();
            assert_eq!(
                claim.kind, "chat_turn",
                "under-cap (plane max=1, current_jobs=0) must enqueue chat_turn: {claim:?}"
            );
            let host = claim
                .payload
                .get("host_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                host,
                hid.to_string(),
                "enqueued chat_turn must be this host: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "under-cap runner_id returned {} {} — must not 400 as at-cap \
                 (control is not a tautology that every runner_id 400s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "under-cap (plane max=1, current_jobs=0) did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }
}

/// Heartbeat `software.updating=true` must 400 POST /chat inside FAIL_CLOSED
/// — not enqueue `chat_turn` / wait 240s. `ensure_online` ignoring
/// `software_updating` hangs while the box has stopped polling.
/// Distinct from pending/offline/held and at-cap siblings.
#[tokio::test]
async fn chat_updating_host_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let (hid, machine) = pair_and_heartbeat_machine(
        &app,
        &jar,
        &ws,
        &format!("updating-chat-{tag}"),
        None,
    )
    .await;

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "software": { "updating": true },
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat software.updating=true: {}",
        hb.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let out = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &jar,
        Some(json!({
            "messages": user_hi(),
            "provider": "grok",
            "runner_id": hid,
        })),
    )
    .await;
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "POST /chat on updating host must be 400 before enqueue, not {} {}",
        out.status,
        out.body
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains("updating"),
        "POST /chat 400 must name updating (not paused/removed/empty): {}",
        out.body
    );
    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "updating runner_id must not enqueue a new chat_turn"
    );
}

/// Owner POST /worker-hosts leftover — pending, no heartbeat.
async fn create_pending_host(app: &axum::Router, jar: &CookieJar, ws: &str, name: &str) -> Uuid {
    let created = call(
        app,
        "POST",
        &hosts_path(ws),
        Some(jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "POST /worker-hosts {name}: {}",
        created.body
    );
    assert_eq!(
        created.body["host"]["status"], "pending",
        "create leftover must stay pending (no pair/heartbeat): {}",
        created.body
    );
    assert!(
        created.body["host"]["last_seen_at"].is_null(),
        "pending leftover must have last_seen_at=null: {}",
        created.body
    );
    Uuid::parse_str(
        created.body["host"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("host.id: {}", created.body)),
    )
    .expect("host uuid")
}

async fn backdate_last_seen_91s(state: &AppState, host_id: Uuid) {
    let n = sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '91 seconds' WHERE id = $1",
    )
    .bind(host_id)
    .execute(&state.db)
    .await
    .expect("backdate last_seen_at");
    assert_eq!(
        n.rows_affected(),
        1,
        "must age last_seen_at on {host_id} (fixture for offline > 90s)"
    );
}

/// `/chat` is JSON 400. `/chat/stream` gates `require_taking` before NDJSON,
/// so this is HTTP 400 — not 200 `type=error` after a hang starts.
fn assert_chat_gate_http_400(out: &HttpOut, what: &str, token: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be HTTP 400 before enqueue/NDJSON, not {} {}",
        out.status,
        out.body
    );
    let ctype = out
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    let err = out.body["error"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        err.contains(token),
        "{what} 400 must name {token}: {}",
        out.body
    );
    assert!(
        !body_has_ok_true(&out.raw),
        "{what} must not include ok:true as a completed CLI turn: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} must not stream a completed turn: {}",
        out.raw
    );
    if let Some(role) = out.body["message"]["role"].as_str() {
        assert_ne!(
            role, "assistant",
            "{what} must not be a completed assistant ChatResponse: {}",
            out.body
        );
    }
}

async fn owner_patch_host(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    host_id: Uuid,
    body: Value,
    label: &str,
) -> HttpOut {
    let out = call(
        app,
        "PATCH",
        &format!("{}/{host_id}", hosts_path(ws)),
        Some(jar),
        Some(body),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "PATCH {label}: {}", out.body);
    out
}

/// Owner POST /worker-hosts leftover — pending, no heartbeat. POST /chat
/// and /chat/stream must 400 inside FAIL_CLOSED and must not enqueue
/// `chat_turn`. Dropping the last_seen-None branch hangs 240s.
#[tokio::test]
async fn chat_pending_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = create_pending_host(&app, &jar, &ws, &format!("pending-{}", Uuid::new_v4())).await;
    let before = chat_turn_count(&state, ws_uuid).await;

    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(&complete, "POST /chat grok+pending", "heartbeated");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(&stream, "POST /chat/stream grok+pending", "heartbeated");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "pending runner_id must not enqueue a new chat_turn"
    );
}

/// Pair+heartbeat, then age last_seen past 90s. Public status is offline.
/// POST /chat and /chat/stream must 400 (offline) and must not enqueue.
#[tokio::test]
async fn chat_offline_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(&app, &jar, &ws, &format!("offline-{}", Uuid::new_v4()), None)
        .await;
    backdate_last_seen_91s(&state, hid).await;
    let got = call(
        &app,
        "GET",
        &format!("{}/{hid}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "GET backdated host: {}", got.body);
    assert_eq!(
        got.body["status"], "offline",
        "last_seen 91s ago must be public offline: {}",
        got.body
    );
    assert_eq!(got.body["paused"], false, "{}", got.body);
    assert_eq!(got.body["drain"], false, "{}", got.body);
    assert_eq!(got.body["enabled"], true, "{}", got.body);

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(&complete, "POST /chat grok+offline", "offline");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(&stream, "POST /chat/stream grok+offline", "offline");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "offline runner_id must not enqueue a new chat_turn"
    );
}

/// Fresh pair+heartbeat, owner PATCH paused=true. Host stays last_seen-fresh.
/// POST /chat and /chat/stream must 400 (not taking) and must not enqueue.
#[tokio::test]
async fn chat_paused_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(&app, &jar, &ws, &format!("paused-{}", Uuid::new_v4()), None)
        .await;
    let paused = owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "paused": true }),
        "paused=true",
    )
    .await;
    assert_eq!(paused.body["paused"], true, "{}", paused.body);
    assert_eq!(paused.body["drain"], false, "{}", paused.body);
    assert_eq!(paused.body["enabled"], true, "{}", paused.body);
    assert!(
        paused.body["last_seen_at"].is_string(),
        "paused leftover must keep last_seen (not pending/offline): {}",
        paused.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(&complete, "POST /chat grok+paused", "not taking");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(&stream, "POST /chat/stream grok+paused", "not taking");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "paused runner_id must not enqueue a new chat_turn"
    );
}

/// Fresh pair+heartbeat, owner PATCH drain=true. Host stays last_seen-fresh.
/// POST /chat and /chat/stream must 400 (not taking) and must not enqueue.
#[tokio::test]
async fn chat_drain_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(&app, &jar, &ws, &format!("drain-{}", Uuid::new_v4()), None).await;
    let drained = owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "drain": true }),
        "drain=true",
    )
    .await;
    assert_eq!(drained.body["drain"], true, "{}", drained.body);
    assert_eq!(drained.body["paused"], false, "{}", drained.body);
    assert_eq!(drained.body["enabled"], true, "{}", drained.body);
    assert!(
        drained.body["last_seen_at"].is_string(),
        "drain leftover must keep last_seen (not pending/offline): {}",
        drained.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(&complete, "POST /chat grok+drain", "not taking");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(&stream, "POST /chat/stream grok+drain", "not taking");

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "drain runner_id must not enqueue a new chat_turn"
    );
}

/// Fresh pair+heartbeat, owner PATCH enabled=false (not DELETE status=disabled).
/// POST /chat and /chat/stream must 400 (not taking) and must not enqueue.
#[tokio::test]
async fn chat_enabled_false_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(
        &app,
        &jar,
        &ws,
        &format!("disabled-{}", Uuid::new_v4()),
        None,
    )
    .await;
    let disabled = owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "enabled": false }),
        "enabled=false",
    )
    .await;
    assert_eq!(disabled.body["enabled"], false, "{}", disabled.body);
    assert_ne!(
        disabled.body["status"], "disabled",
        "enabled=false is not the DELETE leftover (status=disabled): {}",
        disabled.body
    );
    assert_eq!(disabled.body["paused"], false, "{}", disabled.body);
    assert_eq!(disabled.body["drain"], false, "{}", disabled.body);
    assert!(
        disabled.body["last_seen_at"].is_string(),
        "enabled=false leftover must keep last_seen (not pending/offline): {}",
        disabled.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(&complete, "POST /chat grok+enabled=false", "not taking");

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(
        &stream,
        "POST /chat/stream grok+enabled=false",
        "not taking",
    );

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "enabled=false runner_id must not enqueue a new chat_turn"
    );
}

/// Online + accepting is not a tautology that every runner_id 400s.
/// POST /chat must enqueue `chat_turn` for this host inside FAIL_CLOSED.
#[tokio::test]
async fn chat_online_runner_enqueues_chat_turn() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(&app, &jar, &ws, &format!("online-{}", Uuid::new_v4()), None)
        .await;

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let live_app = app.clone();
    let live_jar = jar.clone();
    let live_path = chat_path(&ws);
    let live_handle = tokio::spawn(async move {
        call(
            &live_app,
            "POST",
            &live_path,
            Some(&live_jar),
            Some(live_body),
        )
        .await
    });
    tokio::pin!(live_handle);

    let poll = async {
        loop {
            if let Some(c) = fleet_db::repos::work_claims::claim_next(
                &state.db,
                ws_uuid,
                Uuid::now_v7(),
                chrono::Duration::minutes(1),
            )
            .await
            .expect("claim_next")
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    tokio::select! {
        claim = poll => {
            live_handle.abort();
            assert_eq!(
                claim.kind, "chat_turn",
                "online accepting runner_id must enqueue chat_turn: {claim:?}"
            );
            let host = claim
                .payload
                .get("host_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                host,
                hid.to_string(),
                "enqueued chat_turn must be the online host: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "online accepting runner_id returned {} {} — must not 400 as pending/offline/held \
                 (control is not a tautology that every runner_id 400s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "online accepting runner_id did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }
}

/// Fresh pair+heartbeat, owner PATCH allow_host=false. Host stays last_seen-fresh
/// (not paused/drain/enabled=false). POST /chat and /chat/stream must 400
/// (`effective_host` is allow_host && advertised_host) and must not enqueue.
/// `/me/lease` claim:null is owned by worker_hosts_http.
#[tokio::test]
async fn chat_effective_host_false_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let hid = pair_and_heartbeat(
        &app,
        &jar,
        &ws,
        &format!("allow-host-{}", Uuid::new_v4()),
        None,
    )
    .await;
    let patched = owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "allow_host": false }),
        "allow_host=false",
    )
    .await;
    assert_eq!(patched.body["allow_host"], false, "{}", patched.body);

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(
        &complete,
        "POST /chat grok+allow_host=false",
        "host jobs not allowed",
    );

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(
        &stream,
        "POST /chat/stream grok+allow_host=false",
        "host jobs not allowed",
    );

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "allow_host=false runner_id must not enqueue a new chat_turn"
    );
}

/// Fresh pair+heartbeat, then heartbeat advertised_host=false. POST /chat and
/// /chat/stream must 400 and must not enqueue. both-false is a subset.
#[tokio::test]
async fn chat_advertised_host_false_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let (hid, machine) = pair_and_heartbeat_machine(
        &app,
        &jar,
        &ws,
        &format!("advertised-host-{}", Uuid::new_v4()),
        None,
    )
    .await;

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": false,
            "advertised_container": false,
            "current_jobs": 0,
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat advertised_host=false: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["advertised_host"],
        false,
        "{}",
        hb.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;
    let body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
    assert_chat_gate_http_400(
        &complete,
        "POST /chat grok+advertised_host=false",
        "host jobs not allowed",
    );

    let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
    assert_chat_gate_http_400(
        &stream,
        "POST /chat/stream grok+advertised_host=false",
        "host jobs not allowed",
    );

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "advertised_host=false runner_id must not enqueue a new chat_turn"
    );
}

/// After allow_host=true and advertised_host=true (restored from both off),
/// POST /chat must enqueue `chat_turn` for this host. Distinct from
/// `chat_online_runner_enqueues_chat_turn` (never flipped).
#[tokio::test]
async fn chat_effective_host_restored_enqueues_chat_turn() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let (hid, machine) = pair_and_heartbeat_machine(
        &app,
        &jar,
        &ws,
        &format!("effective-restored-{}", Uuid::new_v4()),
        None,
    )
    .await;

    owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "allow_host": false }),
        "allow_host=false",
    )
    .await;
    let hb_off = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": false,
            "advertised_container": false,
            "current_jobs": 0,
        })),
    )
    .await;
    assert_eq!(
        hb_off.status,
        StatusCode::OK,
        "heartbeat advertised_host=false: {}",
        hb_off.body
    );

    let restored = owner_patch_host(
        &app,
        &jar,
        &ws,
        hid,
        json!({ "allow_host": true }),
        "allow_host=true",
    )
    .await;
    assert_eq!(restored.body["allow_host"], true, "{}", restored.body);
    let hb_on = call_bearer(
        &app,
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
    assert_eq!(
        hb_on.status,
        StatusCode::OK,
        "heartbeat advertised_host=true: {}",
        hb_on.body
    );
    assert_eq!(
        hb_on.body["host"]["advertised_host"],
        true,
        "{}",
        hb_on.body
    );

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": hid,
    });
    let live_app = app.clone();
    let live_jar = jar.clone();
    let live_path = chat_path(&ws);
    let live_handle = tokio::spawn(async move {
        call(
            &live_app,
            "POST",
            &live_path,
            Some(&live_jar),
            Some(live_body),
        )
        .await
    });
    tokio::pin!(live_handle);

    let poll = async {
        loop {
            if let Some(c) = fleet_db::repos::work_claims::claim_next(
                &state.db,
                ws_uuid,
                Uuid::now_v7(),
                chrono::Duration::minutes(1),
            )
            .await
            .expect("claim_next")
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    tokio::select! {
        claim = poll => {
            live_handle.abort();
            assert_eq!(
                claim.kind, "chat_turn",
                "restored effective host must enqueue chat_turn: {claim:?}"
            );
            let host = claim
                .payload
                .get("host_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                host,
                hid.to_string(),
                "enqueued chat_turn must be the restored host: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "restored effective host returned {} {} — must not 400 as !effective_host \
                 (control is not a tautology that every runner_id 400s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "restored effective host did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }
}
