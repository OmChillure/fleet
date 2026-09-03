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
    AppState, ClaudeConfig, GrokConfig, OllamaConfig, OpenaiConfig, ServerConfig, build_http_app,
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

fn ndjson_events(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
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

/// POST /chat/stream cannot resume another workspace's or a teammate's
/// conversation_id. GET/DELETE/complete stay on
/// `list-chat-conversation-user-workspace-scope` (`-- conversation_user_workspace`).
/// Filter `-- stream_conversation` hits this test.
#[tokio::test]
async fn chat_stream_conversation_user_scope() {
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

    // 1. Owner A creates a thread. Snapshot title + messages.
    let created = call(
        &app,
        "POST",
        &conversations_path(&a_ws),
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

    // Plant stored history so a stream that loads list_messages(cid) and
    // echoes it on the SSE (before ensure_conversation) is a visible leak.
    // Do not oneshot chat.rs — this is the HTTP test's DB plant only.
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

    // Same user, second workspace B. Pair a runner there so skipping
    // ensure_conversation would enqueue chat_turn instead of 400 "select a runner".
    let b_created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&a_jar),
        Some(json!({ "name": format!("stream-scope-b-{tag}") })),
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

    // 2. Same user on B's stream with A's conversation_id.
    let other_ws = post_stream_timed(
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
    assert_stream_conversation_fail_closed(
        &other_ws,
        &secrets,
        "same user POST /workspaces/{B}/chat/stream conversation_id=A",
    );
    let after_b = call(
        &app,
        "GET",
        &conversation_path(&a_ws, &a_cid),
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
        "after B stream",
    );
    assert_no_chat_turn(&state, a_uuid, "B stream vs A's workspace").await;
    assert_no_chat_turn(&state, b_uuid, "B stream vs B's workspace").await;

    // 3. Teammate on A who did not create the thread.
    let (member_email, member_jar, _member_ws) = signup_and_login(&app).await;
    let invited = call(
        &app,
        "POST",
        &format!("/api/workspaces/{a_ws}/invites"),
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
    let a_runner = create_paired_online_runner(&app, &a_jar, &a_ws).await;
    let teammate = post_stream_timed(
        &app,
        &member_jar,
        &a_ws,
        json!({
            "messages": [{ "role": "user", "content": unique_hi }],
            "provider": "ollama",
            "conversation_id": a_cid,
            "runner_id": a_runner,
        }),
    )
    .await;
    assert_stream_conversation_fail_closed(
        &teammate,
        &secrets,
        "teammate POST /workspaces/{A}/chat/stream conversation_id=owner",
    );
    let after_teammate = call(
        &app,
        "GET",
        &conversation_path(&a_ws, &a_cid),
        Some(&a_jar),
        None,
    )
    .await;
    assert_owner_conversation_snapshot(
        &after_teammate,
        &a_cid,
        &a_title,
        snap_count,
        &stored_history,
        &unique_hi,
        "after teammate stream",
    );
    assert_no_chat_turn(&state, a_uuid, "teammate stream vs A's workspace").await;

    // 4. Control: owner GET is still 200 with the original title (not a
    // tautology that every conversation GET 404s). Stream without
    // conversation_id still fails closed like disabled providers.
    let control_get = call(
        &app,
        "GET",
        &conversation_path(&a_ws, &a_cid),
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

    for provider in ["ollama", "grok"] {
        let no_cid = post_stream_timed(
            &app,
            &a_jar,
            &a_ws,
            json!({
                "messages": user_hi(),
                "provider": provider
            }),
        )
        .await;
        assert_provider_fail_closed(&no_cid, provider);
    }
}

/// DELETE is a hard delete. POST /chat/stream with that `conversation_id`
/// must 404 or emit only `type=error` (no `type=done`, no assistant text),
/// must not enqueue a `chat_turn`, and must not recreate the row.
/// GET/complete stay on `chat_deleted_conversation_stays_gone`
/// (`-- deleted_conversation` hits both). Other-ws / teammate IDOR stays
/// on `chat_stream_conversation_user_scope`.
#[tokio::test]
async fn chat_stream_deleted_conversation_stays_gone() {
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
        &app,
        "DELETE",
        &conversation_path(&ws, &cid),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body["deleted"], true);

    let listed = call(&app, "GET", &conversations_path(&ws), Some(&jar), None).await;
    assert_deleted_conversation_absent_from_list(&listed, &cid, &secrets, "after DELETE");

    // 4. POST /chat/stream with the same conversation_id is 404, or 200
    //    NDJSON whose events are only type=error. Pair a runner so skipping
    //    ensure_conversation would enqueue chat_turn instead of 400
    //    "select a runner".
    let runner = create_paired_online_runner(&app, &jar, &ws).await;
    for provider in ["grok", "ollama"] {
        let streamed = post_stream_timed(
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
        assert_stream_conversation_fail_closed(
            &streamed,
            &secrets,
            &format!("POST /chat/stream {provider} conversation_id=deleted"),
        );
    }

    let after = call(&app, "GET", &conversation_path(&ws, &cid), Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::NOT_FOUND,
        "GET /conversations/{{cid}} after stream must stay 404 conversation (not 200 with the title): {} {}",
        after.status,
        after.body
    );
    let err = format!("{} {}", after.body, after.raw).to_ascii_lowercase();
    assert!(
        err.contains("conversation"),
        "GET after stream 404 must name conversation: {}",
        after.body
    );
    let hay = format!("{} {}", after.raw, after.body);
    for secret in secrets {
        assert!(
            !hay.contains(secret),
            "GET after stream must not leak `{secret}`: {}",
            after.body
        );
    }

    let listed_after = call(&app, "GET", &conversations_path(&ws), Some(&jar), None).await;
    assert_deleted_conversation_absent_from_list(&listed_after, &cid, &secrets, "after stream");
    assert_deleted_conversation_not_in_db(&state, cid_uuid, ws_uuid, user_id, "after stream").await;
    assert_no_chat_turn(&state, ws_uuid, "stream vs deleted conversation").await;

    // 5. Control: POST /conversations {title: other} still 201 a *new* id ≠ cid.
    let control = call(
        &app,
        "POST",
        &conversations_path(&ws),
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
}

#[tokio::test]
async fn chat_stream_empty_messages_bad_request() {
    // Plane on so whitespace hits prepare_chat_pairs (`no non-empty
    // user/assistant messages`) instead of GROK/OLLAMA_ENABLED=false.
    let (app, _state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;

    let empty = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({ "messages": [], "provider": "ollama" }),
    )
    .await;
    assert_empty_bad_request(&empty, "POST /chat/stream []");

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
    assert_empty_bad_request(&blank, "POST /chat/stream whitespace-only");
}

/// POST /chat and /chat/stream reject role=tool|function|admin|root with
/// 400 `invalid message role` (stream must not 200 NDJSON). Must not persist
/// the planted secret or enqueue a chat_turn. Keep empty-messages above.
/// Distinct from max-32k and unknown-provider. Filter `-- chat_invalid_role`.
#[tokio::test]
async fn chat_invalid_role_stream_rejected_no_persist() {
    let (_base, mut state) = app_with_state().await;
    // Plane-enabled so stream reaches prepare_chat_pairs. Disabled ollama
    // 400s at require_plane_chat_enabled and would make every stream 400
    // a tautology that misses a dropped role gate.
    state.ollama.enabled = true;
    state.ollama.base_url = "http://127.0.0.1:9".into();
    let app = build_router().with_state(state.clone());

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
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_invalid_message_role_http(&complete, role, &secret, "POST /chat");
        let streamed = post_stream_timed(&app, &jar, &ws, body).await;
        assert_invalid_message_role_stream_http(&streamed, role, &secret, "POST /chat/stream");
        secrets.push(secret);
    }

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after invalid-role: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after invalid-role reject: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    let hay = format!("{} {}", after.raw, after.body);
    for secret in &secrets {
        assert!(
            !hay.contains(secret),
            "GET /conversations must not include planted secret {secret}: {}",
            after.body
        );
    }
    if let Some(arr) = after.body["conversations"].as_array() {
        for c in arr {
            if let Some(id) = c["id"].as_str() {
                let got = call(&app, "GET", &conversation_path(&ws, id), Some(&jar), None).await;
                if got.status != StatusCode::OK {
                    continue;
                }
                let detail = format!("{} {}", got.raw, got.body);
                for secret in &secrets {
                    assert!(
                        !detail.contains(secret),
                        "GET /conversations/{id} must not persist planted secret {secret}: {}",
                        got.body
                    );
                }
            }
        }
    }
    assert_no_chat_turn(&state, ws_uuid, "invalid message role stream").await;

    let user_stream = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role_http(&user_stream, "control role=user POST /chat/stream");
    let system_stream = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_invalid_message_role_http(
        &system_stream,
        "control role=system + role=user POST /chat/stream",
    );
    let user_complete = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &jar,
        Some(json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "provider": "ollama",
        })),
    )
    .await;
    assert_not_invalid_message_role_http(&user_complete, "control role=user POST /chat");
}

fn listed_conversation_count(body: &Value) -> usize {
    body["conversations"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("conversations array: {body}"))
}

fn assert_invalid_message_role_http(out: &HttpOut, role: &str, secret: &str, what: &str) {
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
    if let Some(content) = out.body["message"]["content"].as_str() {
        assert!(
            !content.contains(secret),
            "{what} role={role} must not return the planted secret as assistant text: {}",
            out.body
        );
    }
}

fn assert_invalid_message_role_stream_http(out: &HttpOut, role: &str, secret: &str, what: &str) {
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
    assert_invalid_message_role_http(out, role, secret, what);
    assert!(
        !ctype.contains("ndjson") && !ctype.contains("event-stream"),
        "{what} role={role} 400 must not be an NDJSON/SSE stream ({ctype}): {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\"") && !out.raw.contains("\"type\": \"done\""),
        "{what} role={role} must not stream type=done: {}",
        out.raw
    );
}

fn assert_not_invalid_message_role_http(out: &HttpOut, what: &str) {
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

/// POST /chat/stream (and /chat) reject a user line over 32k with HTTP 400
/// `message too long` — not 200 NDJSON that later errors — and persist
/// nothing. Distinct from empty-messages, title-max-72, launch-spec-max-32k.
/// Filter `-- chat_message_max`.
#[tokio::test]
async fn chat_message_max_32k_stream_rejected_no_persist() {
    let (_base, mut state) = app_with_state().await;
    // Plane-enabled so stream reaches prepare_chat_pairs. Disabled ollama
    // 400s at require_plane_chat_enabled and would make every stream 400
    // a tautology that misses a dropped 32k gate.
    state.ollama.enabled = true;
    state.ollama.base_url = "http://127.0.0.1:9".into();
    let app = build_router().with_state(state.clone());

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

    let over_body = json!({
        "messages": [{ "role": "user", "content": huge }],
        "provider": "ollama",
        "runner_id": runner_id,
    });
    let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(over_body.clone())).await;
    assert_message_too_long_http(&complete, &huge, "POST /chat");
    let streamed = post_stream_timed(&app, &jar, &ws, over_body).await;
    assert_message_too_long_stream_http(&streamed, &huge, "POST /chat/stream");

    let after = call(&app, "GET", &conversations, Some(&jar), None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "GET /conversations after 32001-char: {}",
        after.body
    );
    assert_eq!(
        listed_conversation_count(&after.body),
        snap_count,
        "GET /conversations count must be unchanged after 32001-char reject: snap={snap_count} after={} body={}",
        listed_conversation_count(&after.body),
        after.body
    );
    let hay = format!("{} {}", after.raw, after.body);
    assert!(
        !hay.contains(&huge),
        "GET /conversations must not include a new row whose title is the 32001-char string: {}",
        after.body
    );
    if let Some(arr) = after.body["conversations"].as_array() {
        for c in arr {
            if let Some(id) = c["id"].as_str() {
                let got = call(&app, "GET", &conversation_path(&ws, id), Some(&jar), None).await;
                if got.status != StatusCode::OK {
                    continue;
                }
                let detail = format!("{} {}", got.raw, got.body);
                assert!(
                    !detail.contains(&huge),
                    "GET /conversations/{id} must not persist the 32001-char user line: {}",
                    got.status
                );
            }
        }
    }
    assert_no_chat_turn(&state, ws_uuid, "32001-char chat message stream").await;

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
    let resume = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "conversation_id": cid,
            "messages": [{ "role": "user", "content": huge }],
            "provider": "ollama",
            "runner_id": runner_id,
        }),
    )
    .await;
    assert_message_too_long_stream_http(
        &resume,
        &huge,
        "POST /chat/stream with conversation_id + 32001-char user line",
    );
    let got = call(&app, "GET", &conversation_path(&ws, &cid), Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET /conversations/{{id}} after 32001-char stream resume: {}",
        got.body
    );
    let detail = format!("{} {}", got.raw, got.body);
    assert!(
        !detail.contains(&huge),
        "GET /conversations/{{id}} messages must not include the 32001-char string"
    );
    assert_no_chat_turn(&state, ws_uuid, "32001-char stream resume").await;

    let at_cap = "y".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);
    let control_complete = call_timed(
        &app,
        "POST",
        &chat_path(&ws),
        &jar,
        Some(json!({
            "messages": [{ "role": "user", "content": at_cap }],
            "provider": "ollama",
        })),
    )
    .await;
    assert_not_message_too_long_http(&control_complete, "control 32_000-char POST /chat");
    let control_stream = post_stream_timed(
        &app,
        &jar,
        &ws,
        json!({
            "messages": [{ "role": "user", "content": at_cap }],
            "provider": "ollama",
        }),
    )
    .await;
    assert_not_message_too_long_http(&control_stream, "control 32_000-char POST /chat/stream");
}

fn assert_message_too_long_http(out: &HttpOut, huge: &str, what: &str) {
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

/// Other-ws cookie on GET /chat/status, POST /chat, POST /chat/stream is 403
/// before enqueue / the 240s runner wait. Stream-only coverage stays in
/// `chat_stream_other_workspace_forbidden`.
#[tokio::test]
async fn chat_status_complete_stream_other_workspace_cookie_forbidden_before_enqueue() {
    let (app, state) = app_with_state().await;
    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let runner_id = create_paired_online_runner(&app, &a_jar, &a_ws).await;
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let status = call_timed(&app, "GET", &status_path(&a_ws), &b_jar, None).await;
    assert_membership_forbidden(&status, "GET /chat/status");

    let status_runner = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={runner_id}", status_path(&a_ws)),
        &b_jar,
        None,
    )
    .await;
    assert_membership_forbidden(&status_runner, "GET /chat/status?runner_id");

    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": runner_id,
        });
        let complete =
            call_timed(&app, "POST", &chat_path(&a_ws), &b_jar, Some(body.clone())).await;
        assert_membership_forbidden(&complete, &format!("POST /chat {provider}+runner_id"));

        let stream = call_timed(&app, "POST", &stream_path(&a_ws), &b_jar, Some(body)).await;
        assert_membership_forbidden(&stream, &format!("POST /chat/stream {provider}+runner_id"));
    }

    assert_no_chat_turn(&state, a_uuid, "other-ws cookie vs owner workspace").await;
    assert_no_chat_turn(&state, b_uuid, "other-ws cookie vs caller workspace").await;
}

/// A's cookie on A's `/chat/status?runner_id={B}`, POST `/chat`, and
/// POST `/chat/stream` (grok|ollama + B's host) is 404. Must not return
/// B's heartbeat agents/models or enqueue a `chat_turn` / lease on B.
/// Other-ws cookie on B's path is `list-runner-chat-membership` (403).
/// Cluster label is `list-cluster-host-label-undo` — this lock is a
/// non-cluster paired host. Filter `-- foreign_runner`.
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

#[tokio::test]
async fn chat_foreign_runner_id_is_404_and_does_not_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let tag = Uuid::new_v4();
    let b_name = format!("foreign-b-{tag}");
    let b_grok = format!("foreign-b-grok-{tag}");
    let b_ollama = format!("foreign-b-ollama-{tag}");
    let a_grok = format!("foreign-a-grok-{tag}");
    let a_ollama = format!("foreign-a-ollama-{tag}");
    let secrets = [b_name.as_str(), b_grok.as_str(), b_ollama.as_str()];

    let (_a_email, a_jar, a_ws) = signup_and_login(&app).await;
    let (_b_email, b_jar, b_ws) = signup_and_login(&app).await;
    assert_ne!(a_ws, b_ws, "two signups must get distinct workspaces");
    let a_uuid = Uuid::parse_str(&a_ws).expect("a_ws");
    let b_uuid = Uuid::parse_str(&b_ws).expect("b_ws");

    let (b_host, b_machine) = pair_and_heartbeat_machine(
        &app,
        &b_jar,
        &b_ws,
        &b_name,
        Some(planted_ready_agents(&b_grok, &b_ollama)),
    )
    .await;
    assert!(
        b_machine.starts_with("flm_"),
        "pair must mint a real machine token: {b_machine}"
    );

    // Non-cluster paired host — cluster visibility is a different card.
    let b_got = call(
        &app,
        "GET",
        &format!("{}/{b_host}", hosts_path(&b_ws)),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_got.status,
        StatusCode::OK,
        "B GET own host: {}",
        b_got.body
    );
    let labels = b_got.body["labels"].as_array().cloned().unwrap_or_default();
    assert!(
        labels.iter().all(|l| l
            .as_str()
            .map(|s| !s.eq_ignore_ascii_case("cluster"))
            .unwrap_or(true)),
        "this lock is a non-cluster paired host \
         (list-cluster-host-label-undo owns the exception): {}",
        b_got.body
    );
    assert_eq!(
        b_got.body["name"].as_str(),
        Some(b_name.as_str()),
        "B host name: {}",
        b_got.body
    );

    let before_a = chat_turn_count(&state, a_uuid).await;
    let before_b = chat_turn_count(&state, b_uuid).await;

    let status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={b_host}", status_path(&a_ws)),
        &a_jar,
        None,
    )
    .await;
    assert_foreign_runner_404(
        &status,
        &secrets,
        "GET /workspaces/{A}/chat/status?runner_id={B}",
    );

    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": b_host,
        });
        let complete =
            call_timed(&app, "POST", &chat_path(&a_ws), &a_jar, Some(body.clone())).await;
        assert_foreign_runner_404(
            &complete,
            &secrets,
            &format!("POST /workspaces/{{A}}/chat {provider}+B runner_id"),
        );

        let stream = call_timed(&app, "POST", &stream_path(&a_ws), &a_jar, Some(body)).await;
        assert_foreign_runner_404(
            &stream,
            &secrets,
            &format!("POST /workspaces/{{A}}/chat/stream {provider}+B runner_id"),
        );
    }

    assert_eq!(
        chat_turn_count(&state, a_uuid).await,
        before_a,
        "foreign runner_id must not enqueue a chat_turn on A"
    );
    assert_eq!(
        chat_turn_count(&state, b_uuid).await,
        before_b,
        "foreign runner_id must not enqueue a chat_turn on B"
    );
    assert_no_chat_turn(&state, a_uuid, "A after foreign runner_id").await;
    assert_no_chat_turn(&state, b_uuid, "B after foreign runner_id").await;

    let lease = lease_with_machine(&app, &b_machine).await;
    assert_lease_claim_null(&lease, "B /me/lease after A's foreign runner_id chat");

    let still_b = call(
        &app,
        "GET",
        &format!("{}/{b_host}", hosts_path(&b_ws)),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        still_b.status,
        StatusCode::OK,
        "B host must stay paired after foreign chat attempts: {}",
        still_b.body
    );
    assert_ne!(
        still_b.body["status"].as_str(),
        Some("disabled"),
        "foreign chat must not revoke B: {}",
        still_b.body
    );

    // Control: B's own cookie + B's runner_id still advertises the planted
    // inventory so this is not a tautology that every runner_id 404s.
    let b_status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={b_host}", status_path(&b_ws)),
        &b_jar,
        None,
    )
    .await;
    assert_eq!(
        b_status.status,
        StatusCode::OK,
        "B GET /chat/status?runner_id=own must stay 200: {}",
        b_status.body
    );
    assert_eq!(
        b_status.body["available"], true,
        "B's host must stay a ready inventory: {}",
        b_status.body
    );
    let b_raw = b_status.raw.to_ascii_lowercase();
    assert!(
        b_raw.contains(&b_grok.to_ascii_lowercase())
            || b_raw.contains(&b_ollama.to_ascii_lowercase()),
        "B own status must keep heartbeat models: {}",
        b_status.body
    );

    // Control: A's cookie + A's own runner still enqueues chat_turn.
    let a_host = pair_and_heartbeat(
        &app,
        &a_jar,
        &a_ws,
        &format!("foreign-a-{tag}"),
        Some(planted_ready_agents(&a_grok, &a_ollama)),
    )
    .await;
    assert_ne!(a_host, b_host, "A and B hosts must be distinct");

    let a_status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={a_host}", status_path(&a_ws)),
        &a_jar,
        None,
    )
    .await;
    assert_eq!(
        a_status.status,
        StatusCode::OK,
        "A GET /chat/status?runner_id=own: {}",
        a_status.body
    );
    assert_eq!(
        a_status.body["available"], true,
        "A's host must stay a ready inventory: {}",
        a_status.body
    );
    let a_raw = a_status.raw.to_ascii_lowercase();
    assert!(
        a_raw.contains(&a_grok.to_ascii_lowercase())
            || a_raw.contains(&a_ollama.to_ascii_lowercase()),
        "A own status must keep heartbeat models: {}",
        a_status.body
    );
    let a_hay = format!("{} {}", a_status.body, a_status.raw);
    assert!(
        !a_hay.contains(&b_grok) && !a_hay.contains(&b_ollama),
        "A own status must not include B's planted models: {}",
        a_status.body
    );

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": a_host,
    });
    let live_app = app.clone();
    let live_jar = a_jar.clone();
    let live_path = chat_path(&a_ws);
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
                a_uuid,
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
                "A's own runner_id must enqueue chat_turn: {claim:?}"
            );
            let host = claim
                .payload
                .get("host_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                host,
                a_host.to_string(),
                "enqueued chat_turn must be A's host, not B: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "A's own runner_id returned {} {} — must not 404 as foreign \
                 (control is not a tautology that every runner_id 404s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "A's own runner_id did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }

    assert_eq!(
        chat_turn_count(&state, b_uuid).await,
        before_b,
        "A's own-runner control must not enqueue a chat_turn on B"
    );
    assert_no_chat_turn(&state, b_uuid, "B after A's own-runner control").await;
    let later_lease = lease_with_machine(&app, &b_machine).await;
    assert_lease_claim_null(&later_lease, "B /me/lease after A's own-runner control");
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

/// GET /chat/status?runner_id=revoked must not advertise that host's heartbeat
/// as a ready inventory. 404 or available=false / no ready provider is ok.
fn assert_revoked_status_not_ready(out: &HttpOut, planted_models: &[&str]) {
    assert!(
        !out.status.is_server_error(),
        "GET /chat/status for a revoked runner must not 5xx: {} {}",
        out.status,
        out.body
    );
    if out.status == StatusCode::NOT_FOUND {
        return;
    }
    assert!(
        out.status == StatusCode::OK || out.status.is_client_error(),
        "GET /chat/status revoked must be 404, 400, or 200 not-ready, not {}: {}",
        out.status,
        out.body
    );
    if out.status != StatusCode::OK {
        return;
    }
    let available = out.body["available"].as_bool().unwrap_or(true);
    let providers = out.body["providers"].as_array();
    let any_ready = providers
        .map(|ps| ps.iter().any(|p| p["available"].as_bool() == Some(true)))
        .unwrap_or(false);
    assert!(
        !available && !any_ready,
        "revoked runner must not be a ready inventory: {}",
        out.body
    );
    if let Some(ps) = providers {
        for p in ps {
            if p["available"].as_bool() != Some(true) {
                continue;
            }
            let models = p["models"].as_array().cloned().unwrap_or_default();
            for planted in planted_models {
                assert!(
                    !models.iter().any(|m| {
                        m.as_str() == Some(*planted)
                            || m.get("name").and_then(Value::as_str) == Some(*planted)
                    }),
                    "revoked heartbeat model `{planted}` must not be a ready provider model: {}",
                    out.body
                );
            }
        }
    }
}

/// DELETE revoke sets status=disabled and drops the flm_. Chat must 400 quickly
/// and must not enqueue chat_turn — `ensure_online` ignoring status=disabled
/// is the hole (stale last_seen still passes for 90s).
#[tokio::test]
async fn chat_revoked_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let revoked_grok = format!("revoked-lock-grok-{tag}");
    let revoked_ollama = format!("revoked-lock-ollama-{tag}");
    let live_grok = format!("online-lock-grok-{tag}");
    let live_ollama = format!("online-lock-ollama-{tag}");

    let hid = pair_and_heartbeat(
        &app,
        &jar,
        &ws,
        &format!("revoked-{tag}"),
        Some(planted_ready_agents(&revoked_grok, &revoked_ollama)),
    )
    .await;
    let live_id = pair_and_heartbeat(
        &app,
        &jar,
        &ws,
        &format!("online-{tag}"),
        Some(planted_ready_agents(&live_grok, &live_ollama)),
    )
    .await;
    assert_ne!(hid, live_id, "two paired hosts");

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
    assert_eq!(
        revoked.body["status"], "disabled",
        "revoke leftover status: {}",
        revoked.body
    );

    // Leftover stays (Show removed). last_seen is still fresh and enabled is
    // still true — revoke does not PATCH those — so ensure_online's last_seen
    // + enabled && !paused && !drain gate would still pass. This is not the
    // pending / offline / enabled=false card.
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
    assert_eq!(
        leftover.body["paused"], false,
        "revoke must not be the paused held card: {}",
        leftover.body
    );
    assert_eq!(
        leftover.body["drain"], false,
        "revoke must not be the drain held card: {}",
        leftover.body
    );
    assert!(
        leftover.body["last_seen_at"].is_string(),
        "revoke must keep last_seen (not the pending/offline card): {}",
        leftover.body
    );

    let before = chat_turn_count(&state, ws_uuid).await;

    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": hid,
        });
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_revoked_runner_rejected(&complete, &format!("POST /chat {provider}+revoked"));

        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_revoked_runner_rejected(&stream, &format!("POST /chat/stream {provider}+revoked"));
    }

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "revoked runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "revoked runner_id after DELETE").await;

    let status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={hid}", status_path(&ws)),
        &jar,
        None,
    )
    .await;
    assert_revoked_status_not_ready(&status, &[&revoked_grok, &revoked_ollama]);

    // Control: a still-online paired host is not a tautology that every runner_id 400s.
    let live_status = call_timed(
        &app,
        "GET",
        &format!("{}?runner_id={live_id}", status_path(&ws)),
        &jar,
        None,
    )
    .await;
    assert_eq!(
        live_status.status,
        StatusCode::OK,
        "still-online runner status: {}",
        live_status.body
    );
    assert_eq!(
        live_status.body["available"], true,
        "still-online host must stay a ready inventory: {}",
        live_status.body
    );
    let live_raw = live_status.raw.to_ascii_lowercase();
    assert!(
        live_raw.contains(&live_grok.to_ascii_lowercase())
            || live_raw.contains(&live_ollama.to_ascii_lowercase()),
        "still-online status must keep heartbeat models: {}",
        live_status.body
    );

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": live_id,
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
                "still-online runner_id must enqueue chat_turn: {claim:?}"
            );
            let host = claim
                .payload
                .get("host_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                host,
                live_id.to_string(),
                "enqueued chat_turn must be the still-online host, not the revoked one: {}",
                claim.payload
            );
        }
        out = &mut live_handle => {
            let out = out.expect("live POST join");
            panic!(
                "still-online runner_id returned {} {} — must not 400 as revoked \
                 (control is not a tautology that every runner_id 400s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "still-online runner_id did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }
}

/// At-cap runner: POST /chat is 400 before enqueue / the 240s wait.
/// `/chat/stream` starts NDJSON first, so a gate failure is 400 or 200
/// whose events are only `type=error` — never a completed assistant turn.
fn assert_at_job_cap_chat_rejected(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("at cap")
            || err.contains("not taking")
            || err.contains("too many jobs")
            || err.contains("too many"),
        "{what} must name at cap / not taking / too many jobs: {}",
        out.body
    );
    assert!(
        !body_has_ok_true(&out.raw),
        "{what} must not include ok:true as a completed CLI turn: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("\"type\":\"done\""),
        "{what} must not stream a completed turn: {}",
        out.raw
    );
    if out.status == StatusCode::OK {
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
        return;
    }
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 at cap / not taking / too many jobs, \
         or 200 stream type=error only, not a hang or completed assistant turn: {} {}",
        out.status,
        out.body
    );
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

    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": hid,
        });
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_at_job_cap_chat_rejected(&complete, &format!("POST /chat {provider}+at-cap"));

        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_at_job_cap_chat_rejected(&stream, &format!("POST /chat/stream {provider}+at-cap"));
    }

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "at-cap runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "at-cap runner_id after current_jobs=1").await;
}

/// `/chat` and `/chat/stream` must 400 before enqueue / the 240s wait when
/// the runner heartbeated `software.updating=true`. `ensure_online` ignoring
/// that flag hangs 240s while the box has stopped polling.
fn assert_updating_host_chat_rejected(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
        out.status,
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 updating / not taking / host is updating, \
         not a 240s hang or completed assistant turn: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("updating") || err.contains("not taking") || err.contains("host is updating"),
        "{what} 400 must name updating / not taking / host is updating: {}",
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

/// Heartbeat `software.updating=true` must 400 POST /chat and /chat/stream
/// inside FAIL_CLOSED — not enqueue `chat_turn` / wait 240s.
/// Distinct from list-chat-pending-offline-held-runner-no-enqueue
/// (pending / 90s offline / PATCH paused).
#[tokio::test]
async fn chat_updating_host_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let grok_model = format!("updating-grok-{tag}");
    let ollama_model = format!("updating-ollama-{tag}");
    let (hid, machine) = pair_and_heartbeat_machine(
        &app,
        &jar,
        &ws,
        &format!("updating-chat-{tag}"),
        Some(planted_ready_agents(&grok_model, &ollama_model)),
    )
    .await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );
    let before = chat_turn_count(&state, ws_uuid).await;
    let host_path = format!("{}/{hid}", hosts_path(&ws));

    let hb = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": true
            },
            "agents": planted_ready_agents(&grok_model, &ollama_model),
        })),
    )
    .await;
    assert_eq!(
        hb.status,
        StatusCode::OK,
        "heartbeat software.updating=true: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["software"]["updating"], true,
        "heartbeat must persist software.updating: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["paused"], false, "{}", hb.body);
    assert_eq!(hb.body["host"]["drain"], false, "{}", hb.body);
    assert_eq!(hb.body["host"]["enabled"], true, "{}", hb.body);
    assert_ne!(
        hb.body["host"]["status"], "disabled",
        "updating must not disable the host: {}",
        hb.body
    );
    assert_eq!(hb.body["host"]["status"], "online", "{}", hb.body);

    let got = call(&app, "GET", &host_path, Some(&jar), None).await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "GET after updating heartbeat: {}",
        got.body
    );
    assert_eq!(
        got.body["software"]["updating"], true,
        "GET host updating pill: {}",
        got.body
    );
    assert_eq!(got.body["paused"], false, "{}", got.body);
    assert_eq!(got.body["drain"], false, "{}", got.body);
    assert_eq!(got.body["status"], "online", "{}", got.body);

    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": hid,
        });
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_updating_host_chat_rejected(&complete, &format!("POST /chat {provider}+updating"));

        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_updating_host_chat_rejected(
            &stream,
            &format!("POST /chat/stream {provider}+updating"),
        );
    }

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "updating runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(
        &state,
        ws_uuid,
        "updating runner_id after software.updating=true",
    )
    .await;

    // Control: online + updating=false still 400s only for empty messages
    // (not a tautology that every runner_id 400s as updating).
    let hb_clear = call_bearer(
        &app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        &machine,
        Some(json!({
            "advertised_host": true,
            "advertised_container": false,
            "current_jobs": 0,
            "platform": "linux-x86_64",
            "software": {
                "git_sha": "updating-host-swap",
                "updating": false
            },
            "agents": planted_ready_agents(&grok_model, &ollama_model),
        })),
    )
    .await;
    assert_eq!(
        hb_clear.status,
        StatusCode::OK,
        "heartbeat software.updating=false: {}",
        hb_clear.body
    );
    assert_eq!(
        hb_clear.body["host"]["software"]["updating"], false,
        "{}",
        hb_clear.body
    );

    let empty = json!({
        "messages": [],
        "provider": "grok",
        "runner_id": hid,
    });
    let empty_out = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(empty)).await;
    assert_empty_bad_request(&empty_out, "POST /chat empty on online updating=false");
    let empty_err = format!("{} {}", empty_out.body, empty_out.raw).to_ascii_lowercase();
    assert!(
        !empty_err.contains("updating") && !empty_err.contains("not taking"),
        "online empty /chat must 400 as empty, not as updating: {}",
        empty_out.body
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
                "online + updating=false must enqueue chat_turn: {claim:?}"
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
                "online + updating=false returned {} {} — must not 400 as updating \
                 (control is not a tautology that every runner_id 400s)",
                out.status, out.body
            );
        }
        _ = tokio::time::sleep(FAIL_CLOSED) => {
            live_handle.abort();
            panic!(
                "online + updating=false did not enqueue chat_turn within {FAIL_CLOSED:?}"
            );
        }
    }
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

fn assert_not_completed_assistant_turn(out: &HttpOut, what: &str) {
    assert!(
        !out.status.is_server_error(),
        "{what} must not 5xx: {} {}",
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
    if out.status.is_success() {
        if let Some(role) = out.body["message"]["role"].as_str() {
            assert_ne!(
                role, "assistant",
                "{what} must not be a completed assistant ChatResponse: {}",
                out.body
            );
        }
        assert!(
            out.body.get("message").is_none()
                || stream_events(&out.raw)
                    .iter()
                    .any(|e| e.get("type").and_then(Value::as_str) == Some("error")),
            "{what} must not be a completed assistant turn: {}",
            out.body
        );
    }
}

/// `/chat` is JSON 400. `/chat/stream` starts NDJSON first, so a gate
/// failure is 400 or 200 whose events are only `type=error`.
fn assert_pending_offline_held_rejected(out: &HttpOut, what: &str, phrases: &[&str]) {
    assert_not_completed_assistant_turn(out, what);
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        phrases
            .iter()
            .any(|p| err.contains(&p.to_ascii_lowercase())),
        "{what} must name {} (400 before enqueue / 240s wait): {}",
        phrases.join(" / "),
        out.body
    );
    if out.status == StatusCode::OK {
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
        return;
    }
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 (or 200 stream type=error only), not a hang or completed assistant turn: {} {}",
        out.status,
        out.body
    );
}

async fn post_chat_and_stream_gated(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    runner_id: Uuid,
    phrases: &[&str],
    label: &str,
) {
    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": runner_id,
        });
        let complete = call_timed(app, "POST", &chat_path(ws), jar, Some(body.clone())).await;
        assert_pending_offline_held_rejected(
            &complete,
            &format!("POST /chat {provider}+{label}"),
            phrases,
        );
        let stream = call_timed(app, "POST", &stream_path(ws), jar, Some(body)).await;
        assert_pending_offline_held_rejected(
            &stream,
            &format!("POST /chat/stream {provider}+{label}"),
            phrases,
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

/// Pending (no heartbeat), offline (`last_seen` > 90s), and held
/// (paused / drain / `!enabled`) runners must 400 POST /chat and
/// /chat/stream inside FAIL_CLOSED and must not enqueue `chat_turn`.
/// Dropping `ensure_online` on those branches hangs 240s.
#[tokio::test]
async fn chat_pending_offline_held_runner_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let before = chat_turn_count(&state, ws_uuid).await;

    // 1. Owner POST /worker-hosts — pending, no heartbeat.
    let pending_id = create_pending_host(&app, &jar, &ws, &format!("pending-{tag}")).await;
    post_chat_and_stream_gated(
        &app,
        &jar,
        &ws,
        pending_id,
        &["has not heartbeated", "not taking"],
        "pending",
    )
    .await;

    // 2. Pair+heartbeat that host, then age last_seen past the 90s offline window.
    let offline_id = pair_and_heartbeat(&app, &jar, &ws, &format!("offline-{tag}"), None).await;
    backdate_last_seen_91s(&state, offline_id).await;
    let offline_got = call(
        &app,
        "GET",
        &format!("{}/{offline_id}", hosts_path(&ws)),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        offline_got.status,
        StatusCode::OK,
        "GET backdated host: {}",
        offline_got.body
    );
    assert_eq!(
        offline_got.body["status"], "offline",
        "last_seen 91s ago must be public offline: {}",
        offline_got.body
    );
    post_chat_and_stream_gated(&app, &jar, &ws, offline_id, &["offline"], "offline").await;

    // 3. Fresh pair+heartbeat, then owner PATCH paused / drain / enabled=false.
    let paused_id = pair_and_heartbeat(&app, &jar, &ws, &format!("paused-{tag}"), None).await;
    let paused = owner_patch_host(
        &app,
        &jar,
        &ws,
        paused_id,
        json!({ "paused": true }),
        "paused=true",
    )
    .await;
    assert_eq!(paused.body["paused"], true, "{}", paused.body);
    post_chat_and_stream_gated(&app, &jar, &ws, paused_id, &["not taking"], "paused").await;

    let drain_id = pair_and_heartbeat(&app, &jar, &ws, &format!("drain-{tag}"), None).await;
    let drained = owner_patch_host(
        &app,
        &jar,
        &ws,
        drain_id,
        json!({ "drain": true }),
        "drain=true",
    )
    .await;
    assert_eq!(drained.body["drain"], true, "{}", drained.body);
    post_chat_and_stream_gated(&app, &jar, &ws, drain_id, &["not taking"], "drain").await;

    let disabled_id = pair_and_heartbeat(&app, &jar, &ws, &format!("disabled-{tag}"), None).await;
    let disabled = owner_patch_host(
        &app,
        &jar,
        &ws,
        disabled_id,
        json!({ "enabled": false }),
        "enabled=false",
    )
    .await;
    assert_eq!(disabled.body["enabled"], false, "{}", disabled.body);
    post_chat_and_stream_gated(
        &app,
        &jar,
        &ws,
        disabled_id,
        &["not taking"],
        "enabled=false",
    )
    .await;

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before,
        "pending/offline/held runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "pending/offline/held runner_id").await;

    // 4. Control: online + accepting still 400s only for empty messages
    // (keep chat_complete_and_stream_empty_messages_rejected_before_runner_wait)
    // and does not 400 as offline so this is not a tautology that every runner_id 400s.
    let live_id = pair_and_heartbeat(&app, &jar, &ws, &format!("online-{tag}"), None).await;
    let empty_cases = [
        json!({ "messages": [], "provider": "grok", "runner_id": live_id }),
        json!({ "messages": [], "provider": "ollama", "runner_id": live_id }),
    ];
    for body in empty_cases {
        let label = body.to_string();
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_empty_bad_request(&complete, &format!("POST /chat empty on online {label}"));
        let complete_err = format!("{} {}", complete.body, complete.raw).to_ascii_lowercase();
        assert!(
            !complete_err.contains("offline")
                && !complete_err.contains("has not heartbeated")
                && !complete_err.contains("not taking"),
            "online empty /chat must 400 as empty, not as offline/pending/held: {}",
            complete.body
        );
        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_empty_bad_request(
            &stream,
            &format!("POST /chat/stream empty on online {label}"),
        );
        let stream_err = format!("{} {}", stream.body, stream.raw).to_ascii_lowercase();
        assert!(
            !stream_err.contains("offline")
                && !stream_err.contains("has not heartbeated")
                && !stream_err.contains("not taking"),
            "online empty /chat/stream must 400 as empty, not as offline/pending/held: {}",
            stream.body
        );
    }

    let live_body = json!({
        "messages": user_hi(),
        "provider": "grok",
        "runner_id": live_id,
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
                live_id.to_string(),
                "enqueued chat_turn must be the online host, not a pending/offline/held one: {}",
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

/// `/chat` and `/chat/stream` must 400 before enqueue / the 240s wait when
/// effective host mode is off (`allow_host && advertised_host`). `/chat/stream`
/// gates in `require_taking` before NDJSON, so this is HTTP 400 — not 200
/// `type=error` after a hang starts.
fn assert_effective_host_false_rejected(out: &HttpOut, what: &str) {
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{what} must be 400 not taking / host jobs not allowed, \
         not a hang or completed assistant turn: {} {}",
        out.status,
        out.body
    );
    let err = format!("{} {}", out.body, out.raw).to_ascii_lowercase();
    assert!(
        err.contains("not taking") || err.contains("host jobs not allowed"),
        "{what} 400 must name not taking / host jobs not allowed: {}",
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

fn assert_lease_claim_null(lease: &HttpOut, what: &str) {
    assert_eq!(
        lease.status,
        StatusCode::OK,
        "{what} status: {}",
        lease.body
    );
    assert!(
        lease.body["claim"].is_null(),
        "{what} must be 200 {{claim: null}} (list-allow-host-false-lease-null / \
         list-advertised-host-false-lease-null): {}",
        lease.body
    );
    assert!(
        lease.body.get("lease_token").is_none() || lease.body["lease_token"].is_null(),
        "{what} must not return a lease_token: {}",
        lease.body
    );
    let dumped = lease.body.to_string();
    assert!(
        !dumped.contains("lease_token"),
        "{what} must not return a lease_token / payload: {}",
        lease.body
    );
    assert!(
        !dumped.contains("chat_turn"),
        "{what} must not return a chat_turn payload: {}",
        lease.body
    );
}

fn assert_cookie_only_lease_unauthorized(out: &HttpOut) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "cookie-only /me/lease must stay 401 (list-cookie-jwt-rejected-as-machine-token): {} {}",
        out.status,
        out.body
    );
    assert!(
        out.body.get("claim").is_none() || out.body["claim"].is_null(),
        "cookie-only lease must not return a claim: {}",
        out.body
    );
    assert!(
        !out.body.to_string().contains("lease_token"),
        "cookie-only lease must not return a lease_token: {}",
        out.body
    );
}

fn assert_host_online_unheld(got: &Value, hid: Uuid, what: &str) {
    let hid_s = hid.to_string();
    assert_eq!(got["id"].as_str(), Some(hid_s.as_str()), "{what} id: {got}");
    assert_eq!(got["status"], "online", "{what} must stay online: {got}");
    assert_eq!(got["paused"], false, "{what} paused unchanged: {got}");
    assert_eq!(got["drain"], false, "{what} drain unchanged: {got}");
    assert_eq!(got["enabled"], true, "{what} enabled unchanged: {got}");
    let raw = got["last_seen_at"]
        .as_str()
        .unwrap_or_else(|| panic!("{what} last_seen_at: {got}"));
    let seen = chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("{what} last_seen_at parse {raw}: {e}"))
        .with_timezone(&chrono::Utc);
    let age = chrono::Utc::now().signed_duration_since(seen);
    assert!(
        age < chrono::Duration::seconds(30) && age > chrono::Duration::seconds(-5),
        "{what} last_seen must be recent ({age}): {got}"
    );
}

async fn get_host(app: &axum::Router, jar: &CookieJar, ws: &str, hid: Uuid) -> Value {
    let got = call(
        app,
        "GET",
        &format!("{}/{hid}", hosts_path(ws)),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "GET host {hid}: {}", got.body);
    got.body
}

async fn lease_with_machine(app: &axum::Router, machine: &str) -> HttpOut {
    call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/lease",
        machine,
        Some(json!({})),
    )
    .await
}

async fn heartbeat_advertised(
    app: &axum::Router,
    machine: &str,
    advertised_host: bool,
    advertised_container: bool,
    label: &str,
) -> HttpOut {
    let hb = call_bearer(
        app,
        "POST",
        "/api/worker-hosts/me/heartbeat",
        machine,
        Some(json!({
            "advertised_host": advertised_host,
            "advertised_container": advertised_container,
            "current_jobs": 0,
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::OK, "heartbeat {label}: {}", hb.body);
    assert_eq!(
        hb.body["host"]["advertised_host"], advertised_host,
        "heartbeat {label} advertised_host: {}",
        hb.body
    );
    assert_eq!(
        hb.body["host"]["advertised_container"], advertised_container,
        "heartbeat {label} advertised_container: {}",
        hb.body
    );
    hb
}

async fn post_chat_and_stream_effective_host_gated(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    runner_id: Uuid,
    label: &str,
) {
    for provider in ["grok", "ollama"] {
        let body = json!({
            "messages": user_hi(),
            "provider": provider,
            "runner_id": runner_id,
        });
        let complete = call_timed(app, "POST", &chat_path(ws), jar, Some(body.clone())).await;
        assert_effective_host_false_rejected(&complete, &format!("POST /chat {provider}+{label}"));
        let stream = call_timed(app, "POST", &stream_path(ws), jar, Some(body)).await;
        assert_effective_host_false_rejected(
            &stream,
            &format!("POST /chat/stream {provider}+{label}"),
        );
    }
}

/// Effective host mode is `allow_host && advertised_host` (v1 is host-only;
/// `advertised_container` is not a job runtime). `/me/lease` already returns
/// `claim:null` for those boxes, but `ensure_online` never reads the flags —
/// POST /chat|/chat/stream must 400 inside FAIL_CLOSED and must not enqueue
/// `chat_turn` (a 240s composer hang is the hole).
#[tokio::test]
async fn chat_effective_host_false_no_enqueue() {
    let (app, state) = app_with_plane_chat().await;
    let (_email, jar, ws) = signup_and_login(&app).await;
    let ws_uuid = Uuid::parse_str(&ws).expect("ws");
    let tag = Uuid::new_v4();
    let (hid, machine) =
        pair_and_heartbeat_machine(&app, &jar, &ws, &format!("effective-host-{tag}"), None).await;
    assert!(
        machine.starts_with("flm_"),
        "pair must mint a real machine token: {machine}"
    );

    let before = get_host(&app, &jar, &ws, hid).await;
    assert_host_online_unheld(&before, hid, "paired host before allow_host flip");
    assert_eq!(
        before["allow_host"], true,
        "create default allow_host: {before}"
    );
    assert_eq!(
        before["advertised_host"], true,
        "pair+heartbeat advertised_host: {before}"
    );
    let before_turns = chat_turn_count(&state, ws_uuid).await;

    // 1. Owner PATCH allow_host=false. Host stays online (not paused/drain/enabled).
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
    assert_eq!(
        patched.body["advertised_host"], true,
        "advertised_host stays true (list-advertised-host-false-lease-null owns that flip): {}",
        patched.body
    );
    assert_host_online_unheld(&patched.body, hid, "PATCH allow_host=false");

    let got = get_host(&app, &jar, &ws, hid).await;
    assert_eq!(
        got["allow_host"], false,
        "GET after allow_host=false: {got}"
    );
    assert_eq!(got["advertised_host"], true, "GET advertised_host: {got}");
    assert_host_online_unheld(&got, hid, "GET after allow_host=false");

    let lease = lease_with_machine(&app, &machine).await;
    assert_lease_claim_null(&lease, "PATCH allow_host=false /me/lease");

    post_chat_and_stream_effective_host_gated(&app, &jar, &ws, hid, "allow_host=false").await;
    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before_turns,
        "allow_host=false runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "allow_host=false").await;

    // 2. Reset allow_host. Heartbeat advertised_host=false (container-only, then both off).
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
    assert_eq!(
        restored.body["paused"], false,
        "restore is allow_host=true, not paused=false: {}",
        restored.body
    );

    let hb_container = heartbeat_advertised(
        &app,
        &machine,
        false,
        true,
        "advertised_host=false advertised_container=true",
    )
    .await;
    assert_eq!(
        hb_container.body["host"]["allow_host"], true,
        "allow_host still true after advertised flip: {}",
        hb_container.body
    );
    assert_eq!(
        hb_container.body["effective"]["host"], false,
        "effective host is allow_host && advertised_host: {}",
        hb_container.body
    );

    let got = get_host(&app, &jar, &ws, hid).await;
    assert_eq!(
        got["allow_host"], true,
        "GET after advertised_host=false: {got}"
    );
    assert_eq!(
        got["advertised_host"], false,
        "GET advertised_host=false: {got}"
    );
    assert_host_online_unheld(
        &got,
        hid,
        "GET after advertised_host=false advertised_container=true",
    );

    let lease = lease_with_machine(&app, &machine).await;
    assert_lease_claim_null(
        &lease,
        "advertised_host=false advertised_container=true /me/lease",
    );

    post_chat_and_stream_effective_host_gated(
        &app,
        &jar,
        &ws,
        hid,
        "advertised_host=false advertised_container=true",
    )
    .await;
    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before_turns,
        "advertised_host=false runner_id must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(
        &state,
        ws_uuid,
        "advertised_host=false advertised_container=true",
    )
    .await;

    let hb_both =
        heartbeat_advertised(&app, &machine, false, false, "both advertised flags false").await;
    assert_eq!(
        hb_both.body["host"]["allow_host"], true,
        "allow_host still true when both advertised flags are false: {}",
        hb_both.body
    );

    let got = get_host(&app, &jar, &ws, hid).await;
    assert_eq!(
        got["advertised_host"], false,
        "GET both advertised false: {got}"
    );
    assert_eq!(
        got["advertised_container"], false,
        "GET advertised_container=false: {got}"
    );
    assert_host_online_unheld(&got, hid, "GET after both advertised flags false");

    let lease = lease_with_machine(&app, &machine).await;
    assert_lease_claim_null(&lease, "both advertised flags false /me/lease");

    post_chat_and_stream_effective_host_gated(&app, &jar, &ws, hid, "both advertised flags false")
        .await;
    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before_turns,
        "both advertised flags false must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(&state, ws_uuid, "both advertised flags false").await;

    // 3. Control: advertised_host=true + allow_host=true still 400s only for
    // empty messages (keep chat_complete_and_stream_empty_messages_rejected_before_runner_wait)
    // so this is not a tautology that every runner_id 400s.
    heartbeat_advertised(&app, &machine, true, false, "restore advertised_host=true").await;
    let live = get_host(&app, &jar, &ws, hid).await;
    assert_eq!(live["allow_host"], true, "control allow_host: {live}");
    assert_eq!(
        live["advertised_host"], true,
        "control advertised_host: {live}"
    );
    assert_host_online_unheld(&live, hid, "control restored host");

    let empty_cases = [
        json!({ "messages": [], "provider": "grok", "runner_id": hid }),
        json!({ "messages": [], "provider": "ollama", "runner_id": hid }),
    ];
    for body in empty_cases {
        let label = body.to_string();
        let complete = call_timed(&app, "POST", &chat_path(&ws), &jar, Some(body.clone())).await;
        assert_empty_bad_request(
            &complete,
            &format!("POST /chat empty on effective-host {label}"),
        );
        let complete_err = format!("{} {}", complete.body, complete.raw).to_ascii_lowercase();
        assert!(
            !complete_err.contains("not taking") && !complete_err.contains("host jobs not allowed"),
            "restored empty /chat must 400 as empty, not as effective-host-off: {}",
            complete.body
        );
        let stream = call_timed(&app, "POST", &stream_path(&ws), &jar, Some(body)).await;
        assert_empty_bad_request(
            &stream,
            &format!("POST /chat/stream empty on effective-host {label}"),
        );
        let stream_err = format!("{} {}", stream.body, stream.raw).to_ascii_lowercase();
        assert!(
            !stream_err.contains("not taking") && !stream_err.contains("host jobs not allowed"),
            "restored empty /chat/stream must 400 as empty, not as effective-host-off: {}",
            stream.body
        );
    }

    let cookie_lease = call(
        &app,
        "POST",
        "/api/worker-hosts/me/lease",
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_cookie_only_lease_unauthorized(&cookie_lease);

    assert_eq!(
        chat_turn_count(&state, ws_uuid).await,
        before_turns,
        "effective-host-false chat must not enqueue a new chat_turn"
    );
    assert_no_chat_turn(
        &state,
        ws_uuid,
        "after effective-host-false + empty control",
    )
    .await;
}
