//! Grok (xAI) chat via **local Grok CLI auth** — same idea as Claude Code credentials.
//!
//! Primary: `~/.grok/auth.json` (from `grok` / `grok login`)
//! Fallback: `XAI_API_KEY` if no CLI session
//! Model: `grok-4.5` only

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use fleet_auth::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chat::{ChatModelInfo, ChatProviderInfo};
use crate::chat_tools::{
    execute_tool, system_prompt_for_workspace, tool_definitions, ChatAction, MAX_TOOL_ROUNDS,
};
use crate::state::AppState;

const DEFAULT_API_BASE: &str = "https://api.x.ai/v1";
const DEFAULT_OIDC_ISSUER: &str = "https://auth.x.ai";
/// Only model we expose in the UI / chat path.
pub const GROK_MODEL: &str = "grok-4.5";
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
        && let Ok(home) = env::var("HOME") {
            return PathBuf::from(home);
        }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Ok(home) = env::var("HOME") {
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
    Cli {
        access_token: String,
        label: String,
    },
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
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?
        .clone();
    if obj.is_empty() {
        return Err(format!(
            "{} is empty — run `grok login`",
            path.display()
        ));
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
        let exp = entry
            .expires_at
            .as_deref()
            .and_then(parse_expires_at);
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
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
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

/// Resolve Bearer credentials: CLI auth.json (with refresh) → XAI_API_KEY.
async fn resolve_auth(cfg: &GrokConfig) -> Result<AuthSource, String> {
    match load_cli_auth(cfg).await {
        Ok(src) => Ok(src),
        Err(cli_err) => {
            if let Some(key) = cfg.api_key.as_ref().filter(|k| !k.is_empty()) {
                tracing::info!("grok: using XAI_API_KEY fallback (CLI: {cli_err})");
                Ok(AuthSource::ApiKey(key.clone()))
            } else {
                Err(cli_err)
            }
        }
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
                    && entry.expires_at.as_ref().and_then(|s| parse_expires_at(s))
                        .is_some_and(|exp| exp <= chrono::Utc::now())
                {
                    return Err(format!(
                        "Grok CLI session expired and refresh failed: {e}"
                    ));
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

fn grok_models() -> Vec<ChatModelInfo> {
    vec![ChatModelInfo {
        name: GROK_MODEL.into(),
        size: None,
        parameter_size: None,
        family: Some("grok".into()),
    }]
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
            default_model: Some(GROK_MODEL.into()),
        };
    }

    match resolve_auth(cfg).await {
        Ok(AuthSource::Cli { label, .. }) => {
            tracing::info!(
                path = %cfg.auth_path.display(),
                %label,
                "grok provider available (CLI auth)"
            );
            ChatProviderInfo {
                id: "grok".into(),
                name: format!("Grok ({label})"),
                available: true,
                coming_soon: false,
                error: None,
                models: grok_models(),
                default_model: Some(GROK_MODEL.into()),
            }
        }
        Ok(AuthSource::ApiKey(_)) => {
            tracing::info!("grok provider available (XAI_API_KEY)");
            ChatProviderInfo {
                id: "grok".into(),
                name: "Grok (API key)".into(),
                available: true,
                coming_soon: false,
                error: None,
                models: grok_models(),
                default_model: Some(GROK_MODEL.into()),
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
                default_model: Some(GROK_MODEL.into()),
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

async fn chat_completions(
    cfg: &GrokConfig,
    bearer: &str,
    model: &str,
    messages: &[Value],
    with_tools: bool,
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
        tracing::error!(%status, body = %text, model = %model, "grok API error");
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
            && let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                return Err(AppError::bad_request(format!("Grok error: {msg}")));
            }
        return Err(AppError::bad_request(format!(
            "Grok error ({status}): {text}"
        )));
    }

    let parsed: CompletionsResponse = serde_json::from_str(&text).map_err(|e| {
        tracing::error!(error = %e, body = %text, "grok parse failed");
        AppError::internal(format!("grok response parse: {e}"))
    })?;

    if let Some(err) = &parsed.error {
        return Err(AppError::bad_request(format!("Grok error: {}", err.message)));
    }

    Ok(parsed)
}

/// Grok tool loop for fleet sandbox tools (OpenAI-compatible tool_calls).
pub async fn chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &GrokConfig,
    model: &str,
    history: &[(String, String)],
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
    let bearer = auth_bearer(&auth);

    let model = if model.trim().is_empty() {
        GROK_MODEL
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

    let mut actions: Vec<ChatAction> = Vec::new();
    let mut used_model = model.to_string();

    for round in 0..MAX_TOOL_ROUNDS {
        let parsed = chat_completions(cfg, bearer, model, &messages, true).await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model.clone();
        }

        let choice = parsed.choices.first().ok_or_else(|| {
            AppError::internal("grok returned no choices".to_string())
        })?;
        let msg = &choice.message;

        if msg.tool_calls.is_empty() {
            let content = msg.content.as_deref().unwrap_or("").trim().to_string();
            if content.is_empty() && !actions.is_empty() {
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok((used_model, format!("Done.\n{summary}"), actions));
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
            let (result_json, action) =
                execute_tool(state, workspace_id, user_id, name, &args).await;
            actions.push(action);
            let content = serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into());
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": content,
            }));
        }
    }

    let parsed = chat_completions(cfg, bearer, model, &messages, false).await?;
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
    }
    Ok((used_model, content, actions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_grok_45() {
        let m = grok_models();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "grok-4.5");
    }

    #[test]
    fn default_auth_under_home_grok() {
        let p = default_auth_path();
        assert!(p.ends_with("auth.json"));
    }

    #[test]
    fn expand_tilde() {
        if env::var("HOME").is_ok() {
            let p = expand_user_path("~/.grok/auth.json");
            assert!(!p.to_string_lossy().contains('~'));
            assert!(p.ends_with("auth.json"));
        }
    }

    #[test]
    fn parse_expires_rfc3339() {
        let dt = parse_expires_at("2026-08-05T19:43:29.657696501Z");
        assert!(dt.is_some());
    }
}
