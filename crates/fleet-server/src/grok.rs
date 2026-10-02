//! Grok (xAI) chat via **local Grok CLI auth** — same idea as Claude Code credentials.
//!
//! Primary: `~/.grok/auth.json` (from `grok` / `grok login`)
//! Fallback: `XAI_API_KEY` if no CLI session
//! Models come from `grok models` on this box. We do not pin a version.

use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fleet_auth::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::chat::{ChatModelInfo, ChatProgress, ChatProviderInfo};
use crate::chat_tools::{
    ChatAction, ChatLlmContext, MAX_TOOL_ROUNDS, execute_tool_with_retry,
    rewrite_if_asked_to_continue, system_prompt_for_workspace, tool_definitions,
};
use crate::state::AppState;

const DEFAULT_API_BASE: &str = "https://api.x.ai/v1";
const DEFAULT_OIDC_ISSUER: &str = "https://auth.x.ai";
/// First id from `grok models`, or empty when the CLI is missing / logged out.
pub fn default_model() -> String {
    grok_cli_model_names()
        .and_then(|m| m.into_iter().next())
        .unwrap_or_default()
}
/// Refresh a few minutes before expiry (matches CLI early invalidation spirit).
const REFRESH_SKEW_SECS: i64 = 120;

// ── config ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct GrokConfig {
    pub enabled: bool,
    /// `~/.grok/auth.json` (or `$GROK_HOME/auth.json`).
    pub auth_path: PathBuf,
    /// Optional fallback when CLI session is missing.
    pub api_key: Option<String>,
    pub api_base: String,
}

impl GrokConfig {
    pub fn from_env() -> Self {
        let enabled = env::var("GROK_ENABLED")
            .map(|v| {
                let v = v.trim().to_lowercase();
                !(v == "0" || v == "false" || v == "no" || v == "off")
            })
            .unwrap_or(true);

        let auth_path = env::var("GROK_AUTH_PATH")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| expand_user_path(&s))
            .unwrap_or_else(default_auth_path);

        let api_key = env::var("XAI_API_KEY")
            .or_else(|_| env::var("GROK_API_KEY"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let api_base = env::var("XAI_API_BASE")
            .unwrap_or_else(|_| DEFAULT_API_BASE.into())
            .trim_end_matches('/')
            .to_string();

        Self {
            enabled,
            auth_path,
            api_key,
            api_base,
        }
    }
}

fn default_auth_path() -> PathBuf {
    if let Ok(home) = env::var("GROK_HOME") {
        return PathBuf::from(home).join("auth.json");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".grok").join("auth.json");
    }
    PathBuf::from(".grok/auth.json")
}

/// Expand leading `~/` (Rust does not expand shell tildes in env paths).
fn expand_user_path(raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~"
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home);
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(raw)
}

// ── CLI auth.json ───────────────────────────────────────────────────────────

/// One entry under `~/.grok/auth.json` (key is `issuer::client_id`).
#[derive(Debug, Clone, Deserialize, Serialize)]
struct AuthEntry {
    /// Access token (JWT) used as Bearer on api.x.ai.
    key: String,
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    team_name: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    oidc_issuer: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
    /// Preserve unknown fields on write-back after refresh.
    #[serde(flatten)]
    #[serde(default)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
enum AuthSource {
    /// CLI session token from auth.json.
    Cli { access_token: String, label: String },
    /// Explicit API key env.
    ApiKey(String),
}

fn parse_auth_file(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    if !path.is_file() {
        return Err(format!(
            "Grok CLI auth not found at {}. Run `grok login` (or `grok` once to sign in).",
            path.display()
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?
        .clone();
    if obj.is_empty() {
        return Err(format!("{} is empty — run `grok login`", path.display()));
    }
    Ok(obj)
}

fn entry_from_value(v: &Value) -> Result<AuthEntry, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("auth entry: {e}"))
}

fn pick_best_entry(map: &serde_json::Map<String, Value>) -> Result<(String, AuthEntry), String> {
    // Prefer entries with a non-empty key; if multiple, pick the newest expires_at.
    let mut best: Option<(String, AuthEntry, Option<chrono::DateTime<chrono::Utc>>)> = None;
    for (k, v) in map {
        let Ok(entry) = entry_from_value(v) else {
            continue;
        };
        if entry.key.trim().is_empty() {
            continue;
        }
        let exp = entry.expires_at.as_deref().and_then(parse_expires_at);
        match &best {
            None => best = Some((k.clone(), entry, exp)),
            Some((_, _, best_exp)) => {
                // Prefer later expiry (fresher session).
                if exp > *best_exp {
                    best = Some((k.clone(), entry, exp));
                }
            }
        }
    }
    best.map(|(k, e, _)| (k, e))
        .ok_or_else(|| "no valid Grok CLI session in auth.json — run `grok login`".into())
}

fn parse_expires_at(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // RFC3339 with optional fractional seconds / Z.
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn is_expired_or_skew(expires_at: Option<&str>) -> bool {
    let Some(exp) = expires_at.and_then(parse_expires_at) else {
        // No expiry → treat as valid (CLI falls back to long lifetime).
        return false;
    };
    let now = chrono::Utc::now();
    exp <= now + chrono::Duration::seconds(REFRESH_SKEW_SECS)
}

#[derive(Debug, Deserialize)]
struct OidcTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

async fn refresh_oidc_token(entry: &AuthEntry) -> Result<AuthEntry, String> {
    let refresh = entry
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no refresh_token in auth.json — run `grok login`".to_string())?;
    let issuer = entry
        .oidc_issuer
        .as_deref()
        .unwrap_or(DEFAULT_OIDC_ISSUER)
        .trim_end_matches('/');
    let client_id = entry
        .oidc_client_id
        .as_deref()
        .ok_or_else(|| "no oidc_client_id — run `grok login`".to_string())?;

    // Discover token endpoint (or fall back to standard path).
    let token_url = match fetch_token_endpoint(issuer).await {
        Ok(u) => u,
        Err(_) => format!("{issuer}/oauth2/token"),
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let res = client
        .post(&token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id),
        ])
        .send()
        .await
        .map_err(|e| format!("OIDC refresh request failed: {e}"))?;

    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "OIDC refresh failed ({status}): {text}. Run `grok login`."
        ));
    }

    let tok: OidcTokenResponse =
        serde_json::from_str(&text).map_err(|e| format!("OIDC refresh parse: {e}"))?;

    let mut updated = entry.clone();
    updated.key = tok.access_token;
    if let Some(rt) = tok.refresh_token.filter(|s| !s.is_empty()) {
        updated.refresh_token = Some(rt);
    }
    if let Some(secs) = tok.expires_in {
        let exp = chrono::Utc::now() + chrono::Duration::seconds(secs);
        updated.expires_at = Some(exp.to_rfc3339());
    }
    Ok(updated)
}

async fn fetch_token_endpoint(issuer: &str) -> Result<String, String> {
    let url = format!("{issuer}/.well-known/openid-configuration");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("openid-configuration {}", res.status()));
    }
    let v: Value = res.json().await.map_err(|e| e.to_string())?;
    v.get("token_endpoint")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no token_endpoint".into())
}

fn write_auth_entry(path: &Path, entry_key: &str, entry: &AuthEntry) -> Result<(), String> {
    static AUTH_WRITE: Mutex<()> = Mutex::new(());
    let _guard = AUTH_WRITE
        .lock()
        .map_err(|_| "auth.json write lock poisoned".to_string())?;

    let mut map = if path.is_file() {
        parse_auth_file(path)?
    } else {
        serde_json::Map::new()
    };
    let mut val = serde_json::to_value(entry).map_err(|e| e.to_string())?;
    if let Some(obj) = val.as_object_mut() {
        obj.remove("extra");
    }
    map.insert(entry_key.to_string(), val);
    let pretty = serde_json::to_string_pretty(&Value::Object(map)).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(pretty.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.sync_all()
            .map_err(|e| format!("fsync {}: {e}", tmp.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, pretty).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })?;
    Ok(())
}

fn usable_api_key(cfg: &GrokConfig) -> Option<&str> {
    cfg.api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

/// Never send `Authorization: Bearer` with an empty token (would hang on retries).
fn require_bearer(bearer: &str) -> AppResult<&str> {
    let b = bearer.trim();
    if b.is_empty() {
        return Err(AppError::bad_request(
            "Grok auth missing — run `grok login` (or set XAI_API_KEY)".to_string(),
        ));
    }
    Ok(b)
}

fn api_key_fallback(cfg: &GrokConfig, cli_err: String) -> Result<AuthSource, String> {
    if let Some(key) = usable_api_key(cfg) {
        tracing::info!("grok: using XAI_API_KEY fallback (CLI: {cli_err})");
        Ok(AuthSource::ApiKey(key.to_string()))
    } else {
        Err(cli_err)
    }
}

/// Resolve Bearer credentials: CLI auth.json (with refresh) → XAI_API_KEY.
async fn resolve_auth(cfg: &GrokConfig) -> Result<AuthSource, String> {
    match load_cli_auth(cfg).await {
        Ok(src) if !auth_bearer(&src).trim().is_empty() => Ok(src),
        Ok(_) => api_key_fallback(
            cfg,
            "Grok CLI session has an empty token — run `grok login`".into(),
        ),
        Err(cli_err) => api_key_fallback(cfg, cli_err),
    }
}

async fn load_cli_auth(cfg: &GrokConfig) -> Result<AuthSource, String> {
    let map = parse_auth_file(&cfg.auth_path)?;
    let (entry_key, mut entry) = pick_best_entry(&map)?;

    if is_expired_or_skew(entry.expires_at.as_deref()) {
        tracing::info!(
            path = %cfg.auth_path.display(),
            "grok CLI token expired/near-expiry — refreshing via OIDC"
        );
        match refresh_oidc_token(&entry).await {
            Ok(new_entry) => {
                if let Err(e) = write_auth_entry(&cfg.auth_path, &entry_key, &new_entry) {
                    tracing::warn!(error = %e, "could not write refreshed auth.json");
                } else {
                    tracing::info!("grok CLI auth refreshed and saved");
                }
                entry = new_entry;
            }
            Err(e) => {
                // If still have a non-expired token somehow, continue; else fail.
                if is_expired_or_skew(entry.expires_at.as_deref())
                    && entry
                        .expires_at
                        .as_ref()
                        .and_then(|s| parse_expires_at(s))
                        .is_some_and(|exp| exp <= chrono::Utc::now())
                {
                    return Err(format!("Grok CLI session expired and refresh failed: {e}"));
                }
                tracing::warn!(error = %e, "grok refresh failed; trying existing token");
            }
        }
    }

    // Team / org name only — never the operator's email (that leaked into chat status).
    let label = entry
        .team_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "CLI".into());

    Ok(AuthSource::Cli {
        access_token: entry.key,
        label,
    })
}

fn auth_bearer(src: &AuthSource) -> &str {
    match src {
        AuthSource::Cli { access_token, .. } => access_token,
        AuthSource::ApiKey(k) => k,
    }
}

// ── provider status ─────────────────────────────────────────────────────────

/// Parse `grok models` stdout. Default first. Never include secrets.
pub fn parse_grok_models_output(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let t = line.trim();
        let t = t.trim_start_matches('*').trim_start_matches('-').trim();
        let name = t.split_whitespace().next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if !(name.starts_with("grok-") || name.starts_with("sxs-") || name.starts_with("claude-")) {
            continue;
        }
        if !out.iter().any(|m| m == name) {
            out.push(name.to_string());
        }
    }
    out
}

const GROK_MODELS_TIMEOUT: Duration = Duration::from_secs(8);

fn grok_cli_model_names() -> Option<Vec<String>> {
    let mut child = Command::new("grok")
        .arg("models")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                let models = parse_grok_models_output(&stdout);
                return if models.is_empty() {
                    None
                } else {
                    Some(models)
                };
            }
            Ok(None) if start.elapsed() >= GROK_MODELS_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn grok_models() -> Vec<ChatModelInfo> {
    grok_cli_model_names()
        .unwrap_or_default()
        .into_iter()
        .map(|name| ChatModelInfo {
            name,
            size: None,
            parameter_size: None,
            family: Some("grok".into()),
        })
        .collect()
}

fn put_effort(body: &mut Value, effort: Option<&str>) {
    if let Some(e) = effort.map(str::trim).filter(|s| {
        !s.is_empty()
            && s.len() <= 32
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) {
        body["reasoning_effort"] = json!(e);
    }
}

/// Probe local CLI auth (and optional API key) for chat status.
pub async fn provider_status(cfg: &GrokConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "grok".into(),
            name: "Grok (CLI)".into(),
            available: false,
            coming_soon: false,
            error: Some("Grok is disabled (GROK_ENABLED=false)".into()),
            models: vec![],
            default_model: None,
        };
    }

    match resolve_auth(cfg).await {
        Ok(AuthSource::Cli { label, .. }) => {
            tracing::info!(
                path = %cfg.auth_path.display(),
                %label,
                "grok provider available (CLI auth)"
            );
            let models = grok_models();
            let default_model = models.first().map(|m| m.name.clone());
            ChatProviderInfo {
                id: "grok".into(),
                name: format!("Grok ({label})"),
                available: true,
                coming_soon: false,
                error: None,
                models,
                default_model,
            }
        }
        Ok(AuthSource::ApiKey(_)) => {
            tracing::info!("grok provider available (XAI_API_KEY)");
            let models = grok_models();
            let default_model = models.first().map(|m| m.name.clone());
            ChatProviderInfo {
                id: "grok".into(),
                name: "Grok (API key)".into(),
                available: true,
                coming_soon: false,
                error: None,
                models,
                default_model,
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "grok provider offline");
            ChatProviderInfo {
                id: "grok".into(),
                name: "Grok (CLI)".into(),
                available: false,
                coming_soon: false,
                error: Some(e),
                models: vec![],
                default_model: None,
            }
        }
    }
}

// ── OpenAI-compatible chat completions ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CompletionsResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ToolCall {
    id: String,
    #[serde(default)]
    r#type: Option<String>,
    function: ToolFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ToolFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| {
            tracing::error!(error = %e, "grok http client build failed");
            AppError::internal(format!("http client: {e}"))
        })
}

fn tool_args_value(args: &Value) -> Value {
    if let Some(s) = args.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| json!({}))
    } else if args.is_null() {
        json!({})
    } else {
        args.clone()
    }
}

fn arg_keys_preview(args: &Value) -> String {
    match args {
        Value::Object(map) => {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            format!("{} keys", keys.join(","))
        }
        other => format!("type={}", other),
    }
}

async fn emit(progress: &Option<tokio::sync::mpsc::Sender<ChatProgress>>, ev: ChatProgress) {
    if let Some(tx) = progress {
        let _ = tx.send(ev).await;
    }
}

async fn chat_completions(
    cfg: &GrokConfig,
    bearer: &str,
    model: &str,
    messages: &[Value],
    with_tools: bool,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    effort: Option<&str>,
) -> AppResult<CompletionsResponse> {
    let bearer = require_bearer(bearer)?;
    if progress.is_some() {
        return chat_completions_stream(cfg, bearer, model, messages, with_tools, progress, effort)
            .await;
    }
    let effort = effort.map(|s| s.to_string());
    crate::retry::retry_provider("grok", move || {
        let effort = effort.clone();
        async move {
            chat_completions_once(cfg, bearer, model, messages, with_tools, effort.as_deref()).await
        }
    })
    .await
}

async fn chat_completions_once(
    cfg: &GrokConfig,
    bearer: &str,
    model: &str,
    messages: &[Value],
    with_tools: bool,
    effort: Option<&str>,
) -> AppResult<CompletionsResponse> {
    let client = http_client()?;
    let url = format!("{}/chat/completions", cfg.api_base);

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if with_tools {
        body["tools"] = tool_definitions();
    }
    put_effort(&mut body, effort);

    tracing::info!(
        model = %model,
        n_messages = messages.len(),
        with_tools,
        "grok chat request"
    );

    let res = client
        .post(&url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %url, "grok request failed");
            AppError::bad_request(format!("cannot reach xAI API: {e}"))
        })?;

    let status = res.status();
    let text = res.text().await.map_err(|e| {
        tracing::error!(error = %e, "grok body read failed");
        AppError::internal(format!("grok response body: {e}"))
    })?;

    if !status.is_success() {
        let preview: String = text.chars().take(400).collect();
        tracing::error!(%status, body = %preview, model = %model, "grok API error");
        let lower = text.to_lowercase();
        if status.as_u16() == 401 || lower.contains("unauthorized") {
            return Err(AppError::bad_request(
                "Grok auth failed — run `grok login` (or check XAI_API_KEY fallback)".to_string(),
            ));
        }
        if status.as_u16() == 404 || lower.contains("model") {
            return Err(AppError::bad_request(format!(
                "Grok model `{model}` not found or not available on your account"
            )));
        }
        if status.as_u16() == 429 || lower.contains("rate") {
            return Err(AppError::bad_request(
                "Grok rate limited — try again shortly".to_string(),
            ));
        }
        if let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str())
        {
            return Err(AppError::bad_request(format!("Grok error: {msg}")));
        }
        let preview: String = text.chars().take(400).collect();
        return Err(AppError::bad_request(format!(
            "Grok error ({status}): {preview}"
        )));
    }

    let parsed: CompletionsResponse = serde_json::from_str(&text).map_err(|e| {
        let preview: String = text.chars().take(400).collect();
        tracing::error!(error = %e, body = %preview, "grok parse failed");
        AppError::internal(format!("grok response parse: {e}"))
    })?;

    if let Some(err) = &parsed.error {
        return Err(AppError::bad_request(format!(
            "Grok error: {}",
            err.message
        )));
    }

    Ok(parsed)
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamToolDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFnDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamFnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

struct AccTool {
    id: String,
    name: String,
    arguments: String,
}

async fn chat_completions_stream(
    cfg: &GrokConfig,
    bearer: &str,
    model: &str,
    messages: &[Value],
    with_tools: bool,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
    effort: Option<&str>,
) -> AppResult<CompletionsResponse> {
    let client = http_client()?;
    let url = format!("{}/chat/completions", cfg.api_base);
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    if with_tools {
        body["tools"] = tool_definitions();
    }
    put_effort(&mut body, effort);
    let mut res = client
        .post(&url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::bad_request(format!("cannot reach xAI API: {e}")))?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(AppError::bad_request(format!(
            "Grok error ({status}): {}",
            text.chars().take(400).collect::<String>()
        )));
    }

    let mut buf = String::new();
    let mut content = String::new();
    let mut used_model = model.to_string();
    let mut tools: Vec<AccTool> = Vec::new();
    let mut saw_tools = false;
    let mut emit_text = true;

    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| AppError::internal(format!("grok stream chunk: {e}")))?
    {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find('\n') {
            let line = buf[..idx].trim().to_string();
            buf = buf[idx + 1..].to_string();
            let data = line.strip_prefix("data:").map(str::trim).unwrap_or(&line);
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) else {
                continue;
            };
            if !parsed.model.is_empty() {
                used_model = parsed.model;
            }
            for choice in parsed.choices {
                if !choice.delta.tool_calls.is_empty() {
                    saw_tools = true;
                    emit_text = false;
                    for d in choice.delta.tool_calls {
                        while tools.len() <= d.index {
                            tools.push(AccTool {
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                        }
                        let acc = &mut tools[d.index];
                        if let Some(id) = d.id {
                            acc.id = id;
                        }
                        if let Some(f) = d.function {
                            if let Some(n) = f.name {
                                acc.name = n;
                            }
                            if let Some(a) = f.arguments {
                                acc.arguments.push_str(&a);
                            }
                        }
                    }
                }
                if let Some(piece) = choice.delta.content {
                    if !piece.is_empty() {
                        content.push_str(&piece);
                        if emit_text && !saw_tools {
                            if let Some(tx) = progress {
                                let _ = tx
                                    .send(ChatProgress::Delta {
                                        text: piece.clone(),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
        }
    }

    let tool_calls: Vec<ToolCall> = tools
        .into_iter()
        .filter(|t| !t.name.is_empty() || !t.id.is_empty())
        .map(|t| ToolCall {
            id: if t.id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                t.id
            },
            r#type: Some("function".into()),
            function: ToolFunction {
                name: t.name,
                arguments: Value::String(t.arguments),
            },
        })
        .collect();

    Ok(CompletionsResponse {
        model: used_model,
        choices: vec![Choice {
            message: ChoiceMessage {
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                tool_calls,
            },
        }],
        error: None,
    })
}

/// One-shot JSON/text completion (no tools). Used by the case wake loop.
pub async fn complete_text(
    cfg: &GrokConfig,
    model: &str,
    system: &str,
    user: &str,
) -> AppResult<String> {
    if !cfg.enabled {
        return Err(AppError::bad_request(
            "Grok is disabled (set GROK_ENABLED=true)".to_string(),
        ));
    }
    let auth = resolve_auth(cfg).await.map_err(AppError::bad_request)?;
    let bearer = require_bearer(auth_bearer(&auth))?;
    let fallback = default_model();
    let model = if model.trim().is_empty() {
        if fallback.is_empty() {
            return Err(AppError::bad_request(
                "no Grok model from `grok models` — run `grok login`".to_string(),
            ));
        }
        fallback.as_str()
    } else {
        model.trim()
    };
    let messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": user }),
    ];
    let parsed = chat_completions(cfg, bearer, model, &messages, false, None, None).await?;
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        return Err(AppError::bad_request("grok returned empty decision"));
    }
    Ok(content)
}

/// Grok tool loop for fleet sandbox tools (OpenAI-compatible tool_calls).
pub async fn chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &GrokConfig,
    model: &str,
    history: &[(String, String)],
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
    runner_id: Option<Uuid>,
    effort: Option<String>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    if !cfg.enabled {
        return Err(AppError::bad_request(
            "Grok is disabled (set GROK_ENABLED=true)".to_string(),
        ));
    }

    let auth = resolve_auth(cfg).await.map_err(|e| {
        tracing::warn!(error = %e, "grok auth");
        AppError::bad_request(e)
    })?;
    let bearer = require_bearer(auth_bearer(&auth))?;

    let fallback = default_model();
    let model = if model.trim().is_empty() {
        if fallback.is_empty() {
            return Err(AppError::bad_request(
                "no Grok model from `grok models` — run `grok login`".to_string(),
            ));
        }
        fallback.as_str()
    } else {
        model.trim()
    };

    let mut messages: Vec<Value> = Vec::with_capacity(history.len() + 1);
    let system = system_prompt_for_workspace(state, workspace_id).await;
    messages.push(json!({
        "role": "system",
        "content": system,
    }));
    for (role, content) in history {
        let role = if role == "assistant" {
            "assistant"
        } else if role == "system" {
            "system"
        } else {
            "user"
        };
        messages.push(json!({ "role": role, "content": content }));
    }

    emit(
        &progress,
        ChatProgress::Status {
            text: format!("Thinking with {model}…"),
        },
    )
    .await;

    let mut actions: Vec<ChatAction> = Vec::new();
    let mut used_model = model.to_string();

    let mut max_rounds = MAX_TOOL_ROUNDS;
    let mut round = 0usize;
    while round < max_rounds {
        let parsed = chat_completions(
            cfg,
            bearer,
            model,
            &messages,
            true,
            progress.as_ref(),
            effort.as_deref(),
        )
        .await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model.clone();
        }

        let choice = parsed
            .choices
            .first()
            .ok_or_else(|| AppError::internal("grok returned no choices".to_string()))?;
        let msg = &choice.message;

        if msg.tool_calls.is_empty() {
            let mut content = msg.content.as_deref().unwrap_or("").trim().to_string();
            if let Some(rewritten) = rewrite_if_asked_to_continue(&content, &actions) {
                content = rewritten;
            }
            if content.is_empty() && !actions.is_empty() {
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = format!("Done.\n{summary}");
                emit(
                    &progress,
                    ChatProgress::Delta {
                        text: content.clone(),
                    },
                )
                .await;
                return Ok((used_model, content, actions));
            }
            return Ok((used_model, content, actions));
        }

        messages.push(json!({
            "role": "assistant",
            "content": msg.content,
            "tool_calls": msg.tool_calls,
        }));

        for call in &msg.tool_calls {
            let name = call.function.name.trim();
            let args = tool_args_value(&call.function.arguments);
            tracing::info!(
                round,
                tool = %name,
                arg_keys = %arg_keys_preview(&args),
                "grok tool call"
            );
            let llm = ChatLlmContext {
                provider: "grok".into(),
                model: model.to_string(),
                runner_id,
                effort: effort.clone(),
            };
            let prior: Vec<Value> = actions.iter().filter_map(|a| a.detail.clone()).collect();
            emit(
                &progress,
                ChatProgress::Status {
                    text: format!("Using {name}…"),
                },
            )
            .await;
            let (result_json, action) =
                execute_tool_with_retry(state, workspace_id, user_id, name, &args, &llm, &prior)
                    .await;
            emit(&progress, ChatProgress::from_action(&action)).await;
            actions.push(action);
            let content = serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into());
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": content,
            }));
        }
        let n = msg.tool_calls.len();
        let failed: Vec<&ChatAction> = actions.iter().rev().take(n).filter(|a| !a.ok).collect();
        if !failed.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": crate::retry::fail_nudge(&failed),
            }));
            let extra = max_rounds - MAX_TOOL_ROUNDS;
            if extra < crate::retry::EXTRA_ROUNDS_ON_FAIL {
                max_rounds += 1;
            }
        }
        round += 1;
    }

    let parsed = chat_completions(
        cfg,
        bearer,
        model,
        &messages,
        false,
        progress.as_ref(),
        effort.as_deref(),
    )
    .await?;
    if !parsed.model.is_empty() {
        used_model = parsed.model;
    }
    let mut content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        let summary = actions
            .iter()
            .map(|a| format!("• {}: {}", a.tool, a.summary))
            .collect::<Vec<_>>()
            .join("\n");
        content = format!("Reached tool round limit.\n{summary}");
        emit(
            &progress,
            ChatProgress::Delta {
                text: content.clone(),
            },
        )
        .await;
    }
    Ok((used_model, content, actions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Serializes env mutation in this module so parallel tests do not race.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_env<T>(pairs: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<(&str, Option<String>)> =
            pairs.iter().map(|(k, _)| (*k, env::var(k).ok())).collect();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            for (k, v) in pairs {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            for (k, v) in previous {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    #[test]
    fn parse_grok_models_lists_46_first() {
        let raw = "You are logged in with grok.com.\n\nDefault model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n  - sxs-claude-opus-4-6\n";
        assert_eq!(
            parse_grok_models_output(raw),
            vec!["grok-4.6", "grok-4.5", "sxs-claude-opus-4-6"]
        );
    }

    #[test]
    fn default_auth_under_home_grok() {
        with_env(
            &[("GROK_HOME", None), ("HOME", Some("/tmp/fleet-home"))],
            || {
                assert_eq!(
                    default_auth_path(),
                    PathBuf::from("/tmp/fleet-home")
                        .join(".grok")
                        .join("auth.json")
                );
            },
        );
    }

    #[test]
    fn default_auth_prefers_grok_home_over_home() {
        with_env(
            &[
                ("GROK_HOME", Some("/tmp/fleet-grok-home")),
                ("HOME", Some("/tmp/other")),
            ],
            || {
                assert_eq!(
                    default_auth_path(),
                    PathBuf::from("/tmp/fleet-grok-home/auth.json")
                );
            },
        );
    }

    #[test]
    fn default_auth_without_home_or_grok_home_is_relative_dot_grok() {
        with_env(&[("GROK_HOME", None), ("HOME", None)], || {
            assert_eq!(default_auth_path(), PathBuf::from(".grok/auth.json"));
        });
    }

    #[test]
    fn from_env_blank_grok_auth_path_falls_through_to_default() {
        for blank in ["", "   "] {
            with_env(
                &[
                    ("GROK_AUTH_PATH", Some(blank)),
                    ("GROK_HOME", None),
                    ("HOME", Some("/tmp/fleet-home")),
                ],
                || {
                    assert_eq!(
                        GrokConfig::from_env().auth_path,
                        PathBuf::from("/tmp/fleet-home")
                            .join(".grok")
                            .join("auth.json")
                    );
                },
            );
        }
    }

    #[test]
    fn expand_tilde() {
        with_env(&[("HOME", Some("/tmp/fleet-home"))], || {
            assert_eq!(
                expand_user_path("~/.grok/auth.json"),
                PathBuf::from("/tmp/fleet-home").join(".grok/auth.json")
            );
        });
    }

    #[test]
    fn expand_user_path_non_tilde_is_literal() {
        assert_eq!(
            expand_user_path("/var/lib/grok/auth.json"),
            PathBuf::from("/var/lib/grok/auth.json")
        );
        assert_eq!(
            expand_user_path("rel/auth.json"),
            PathBuf::from("rel/auth.json")
        );
    }

    #[test]
    fn expand_user_path_trims_then_expands() {
        let home = env::var("HOME").expect("HOME");
        assert_eq!(
            expand_user_path("  ~/.grok/auth.json\n"),
            PathBuf::from(home).join(".grok/auth.json")
        );
    }

    #[test]
    fn parse_expires_rfc3339() {
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 8, 5)
            .unwrap()
            .and_hms_nano_opt(19, 43, 29, 657_696_501)
            .unwrap()
            .and_utc();
        assert_eq!(
            parse_expires_at("2026-08-05T19:43:29.657696501Z"),
            Some(expected)
        );
        // refresh_oidc_token writes expires_at via to_rfc3339 (+00:00, not Z).
        assert_eq!(parse_expires_at(&expected.to_rfc3339()), Some(expected));
    }

    #[test]
    fn parse_expires_at_rejects_empty_and_garbage() {
        for s in ["", "not-a-date", "2026-08-05"] {
            assert_eq!(parse_expires_at(s), None, "{s:?}");
        }
    }

    #[test]
    fn is_expired_or_skew_missing_or_unparseable_is_fresh() {
        assert!(!is_expired_or_skew(None));
        assert!(!is_expired_or_skew(Some("bogus")));
    }

    #[test]
    fn pick_best_entry_prefers_later_rfc3339_expiry() {
        let mut map = serde_json::Map::new();
        map.insert(
            "early".into(),
            json!({
                "key": "tok-early",
                "expires_at": "2026-08-05T19:43:29.657696501Z",
            }),
        );
        map.insert(
            "late".into(),
            json!({
                "key": "tok-late",
                "expires_at": "2026-08-06T19:43:29.657696501Z",
            }),
        );
        let (name, entry) = pick_best_entry(&map).expect("two non-empty keys");
        assert_eq!(name, "late");
        assert_eq!(entry.key, "tok-late");
    }

    /// Missing CLI session + no usable API key. `api_base` is not api.x.ai so a
    /// regression that sends HTTP cannot reach a live LLM.
    fn cfg_no_usable_auth(api_key: Option<String>) -> GrokConfig {
        GrokConfig {
            enabled: true,
            auth_path: std::env::temp_dir()
                .join(format!("fleet-grok-no-auth-{}/auth.json", Uuid::new_v4())),
            api_key,
            api_base: "http://127.0.0.1:1".into(),
        }
    }

    #[tokio::test]
    async fn missing_or_empty_api_key_fails_closed_without_hang() {
        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        for api_key in [None, Some(String::new())] {
            let cfg = cfg_no_usable_auth(api_key);
            let text =
                tokio::time::timeout(FAIL_CLOSED, complete_text(&cfg, "grok-4", "sys", "ping"))
                    .await
                    .expect("complete_text without key must fail immediately, not hang");
            let err = text.expect_err("must not call xAI with empty Authorization");
            assert!(
                !err.to_string()
                    .to_ascii_lowercase()
                    .contains("cannot reach"),
                "must not send HTTP with empty Authorization: {err}"
            );
        }
    }

    #[tokio::test]
    async fn whitespace_only_api_key_fails_closed_without_hang() {
        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let cfg = cfg_no_usable_auth(Some(" \t ".into()));
        let text = tokio::time::timeout(FAIL_CLOSED, complete_text(&cfg, "grok-4", "sys", "ping"))
            .await
            .expect("whitespace-only Grok key must fail immediately, not hang");
        let err = text.expect_err("must not treat whitespace as a Bearer");
        assert!(
            !err.to_string()
                .to_ascii_lowercase()
                .contains("cannot reach"),
            "must not send HTTP with trimmed-empty Authorization: {err}"
        );
    }

    #[tokio::test]
    async fn expired_cli_session_without_refresh_or_key_fails_closed_without_oidc() {
        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let auth_dir = std::env::temp_dir().join(format!("fleet-grok-expired-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&auth_dir).expect("temp auth dir");
        let auth_path = auth_dir.join("auth.json");
        std::fs::write(
            &auth_path,
            json!({
                "issuer::client": {
                    "key": "expired-session-token",
                    "expires_at": "2020-01-01T00:00:00Z",
                }
            })
            .to_string(),
        )
        .expect("write expired auth.json");
        let cfg = GrokConfig {
            enabled: true,
            auth_path,
            api_key: None,
            api_base: "http://127.0.0.1:1".into(),
        };
        let text = tokio::time::timeout(FAIL_CLOSED, complete_text(&cfg, "grok-4", "sys", "ping"))
            .await
            .expect("expired CLI session without refresh must fail immediately, not hit OIDC");
        let err = text.expect_err("must not refresh via auth.x.ai or call xAI without a key");
        let lower = err.to_string().to_ascii_lowercase();
        assert!(
            !lower.contains("cannot reach"),
            "must not send HTTP before fail-closed: {err}"
        );
        assert!(
            !lower.contains("auth.x.ai"),
            "must not reach fetch_token_endpoint / auth.x.ai: {err}"
        );
        let _ = std::fs::remove_dir_all(&auth_dir);
    }

    #[tokio::test]
    async fn chat_completions_empty_bearer_fails_closed_before_retry() {
        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let cfg = cfg_no_usable_auth(None);
        let messages = [json!({ "role": "user", "content": "ping" })];
        let result = tokio::time::timeout(
            FAIL_CLOSED,
            chat_completions(&cfg, "", "grok-4", &messages, false, None, None),
        )
        .await
        .expect("empty bearer must fail before chat/completions retries");
        let err = result.expect_err("empty bearer must not reach HTTP");
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "expected auth-missing bad_request, got {err}"
        );
        assert!(
            !err.to_string()
                .to_ascii_lowercase()
                .contains("cannot reach"),
            "must not retry HTTP with empty Authorization: {err}"
        );
    }
}
