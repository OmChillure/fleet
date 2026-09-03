//! Workspace chat — multi-provider (Grok / xAI + local Ollama).
//!
//! | Method | Path | Role |
//! | --- | --- | --- |
//! | `GET` | `/api/workspaces/:id/chat/status` | Runner heartbeat inventory (`?runner_id=`) |
//! | `GET` | `/api/workspaces/:id/chat/conversations` | List saved chats |
//! | `POST` | `/api/workspaces/:id/chat/conversations` | Create empty chat |
//! | `GET` | `/api/workspaces/:id/chat/conversations/:cid` | Load chat + messages |
//! | `DELETE` | `/api/workspaces/:id/chat/conversations/:cid` | Delete chat |
//! | `POST` | `/api/workspaces/:id/chat` | Complete turn (persist + tools) |
//!
//! See [`crate::chat_tools`] and [`crate::grok`].

use std::env;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::{chats as chat_repo, workspaces as workspace_repo};
use fleet_db::User;
use fleet_db::WorkerAgentRuntime;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chat_tools::{
    execute_tool_with_retry, rewrite_if_asked_to_continue, system_prompt_for_workspace,
    system_prompt_with_tools, tool_definitions, ChatAction, ChatLlmContext, MAX_TOOL_ROUNDS,
};
use crate::state::AppState;

// ── config ──────────────────────────────────────────────────────────────────

/// Local Ollama connection settings (`OLLAMA_*` env).
#[derive(Clone, Debug)]
pub struct OllamaConfig {
    /// e.g. `http://127.0.0.1:11434`
    pub base_url: String,
    /// Preferred model when the client does not pick one (optional).
    pub default_model: Option<String>,
    pub timeout: Duration,
    /// When false, chat endpoints return a clear "disabled" error.
    pub enabled: bool,
}

impl OllamaConfig {
    pub fn from_env() -> Self {
        let base_url = env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into())
            .trim_end_matches('/')
            .to_string();

        let default_model = env::var("OLLAMA_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let timeout_secs = env::var("OLLAMA_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);

        // OLLAMA_ENABLED=false / 0 / no / off
        let enabled = env::var("OLLAMA_ENABLED")
            .map(|v| {
                let v = v.trim().to_lowercase();
                !(v == "0" || v == "false" || v == "no" || v == "off")
            })
            .unwrap_or(true);

        Self {
            base_url,
            default_model,
            timeout: Duration::from_secs(timeout_secs),
            enabled,
        }
    }

    /// Cookie JSON: scheme/host/port/path only — never `user:pass@`.
    pub fn public_base_url(&self) -> String {
        redact_url_userinfo(&self.base_url)
    }
}

/// Strip userinfo from an http(s) URL. Keep scheme, host, port, path, query.
fn redact_url_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some((_, hostport)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let mut out = String::with_capacity(url.len() - (authority.len() - hostport.len()));
    out.push_str(&url[..scheme_end + 3]);
    out.push_str(hostport);
    out.push_str(&rest[authority_end..]);
    out
}

// ── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/chat/status",
            get(chat_status),
        )
        .route(
            "/api/workspaces/{workspace_id}/chat/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/api/workspaces/{workspace_id}/chat/conversations/{conversation_id}",
            get(get_conversation).delete(delete_conversation),
        )
        .route("/api/workspaces/{workspace_id}/chat", post(chat_complete))
        .route(
            "/api/workspaces/{workspace_id}/chat/stream",
            post(chat_stream),
        )
}

// ── request / response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// Full conversation so far (user + assistant turns). Last message should be user.
    pub messages: Vec<ChatMessageIn>,
    /// Provider id (`ollama`, `grok`, …).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model name as returned by the provider list (e.g. `llama3.2:latest`, `grok-4.5`).
    #[serde(default)]
    pub model: Option<String>,
    /// When set, continue this saved conversation (must belong to the user).
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    /// Self-hosted runner that should take launched agent work.
    #[serde(default)]
    pub runner_id: Option<Uuid>,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessageIn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub message: ChatMessageOut,
    pub model: String,
    pub provider: String,
    /// Tool invocations performed during this turn (sandbox/fleet actions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ChatAction>,
    /// Saved conversation id (created on first turn if omitted).
    pub conversation_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ChatMessageOut {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateConversationBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// One chat backend the UI can pick in the composer.
#[derive(Debug, Clone, Serialize)]
pub struct ChatProviderInfo {
    pub id: String,
    pub name: String,
    /// Ready to accept chat requests.
    pub available: bool,
    /// Shown in the dropdown but not selectable yet.
    pub coming_soon: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Models from the live provider list (Ollama: same as `ollama list` / `/api/tags`).
    pub models: Vec<ChatModelInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatModelInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatStatusResponse {
    /// True if at least one provider can serve chat right now.
    pub available: bool,
    pub enabled: bool,
    /// Ollama hint for older clients. Userinfo is stripped.
    pub base_url: String,
    pub default_model: Option<String>,
    /// Flat list of Ollama models (same as providers[ollama].models) for older clients.
    pub models: Vec<ChatModelInfo>,
    /// Provider catalog for the composer dropdowns.
    pub providers: Vec<ChatProviderInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub hint: String,
}

// ── Ollama HTTP ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    details: Option<TagDetails>,
}

#[derive(Debug, Deserialize)]
struct TagDetails {
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    family: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    model: String,
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OllamaToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    function: OllamaToolFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OllamaToolFunction {
    name: String,
    /// Ollama may return a JSON object or a JSON string.
    #[serde(default)]
    arguments: Value,
}

async fn require_member(state: &AppState, jar: &CookieJar, workspace_id: Uuid) -> AppResult<User> {
    let auth = AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    };
    let user = require_user(&auth, jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    tracing::debug!(%workspace_id, user_id = %user.id, %role, "chat: workspace member ok");
    Ok(user)
}

fn title_from_user_text(text: &str) -> String {
    let t = text.trim().replace('\n', " ");
    let chars: Vec<char> = t.chars().collect();
    if chars.is_empty() {
        return "New chat".into();
    }
    if chars.len() <= 72 {
        return t;
    }
    format!("{}…", chars.into_iter().take(69).collect::<String>())
}

// ── conversation CRUD ───────────────────────────────────────────────────────

async fn list_conversations(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let user = require_member(&state, &jar, workspace_id).await?;
    let list = chat_repo::list_conversations(&state.db, workspace_id, user.id, 100)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({ "conversations": list })))
}

async fn create_conversation(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateConversationBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let user = require_member(&state, &jar, workspace_id).await?;
    // Same 72-char clamp as auto-derived titles (empty/whitespace → "New chat").
    let title = title_from_user_text(body.title.as_deref().unwrap_or(""));
    let id = Uuid::now_v7();
    let now = Utc::now();
    let conv = chat_repo::create_conversation(
        &state.db,
        id,
        workspace_id,
        user.id,
        &title,
        body.provider.as_deref(),
        body.model.as_deref(),
        now,
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(json!({ "conversation": conv }))))
}

async fn get_conversation(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let user = require_member(&state, &jar, workspace_id).await?;
    let conv = chat_repo::get_conversation(&state.db, conversation_id, workspace_id, user.id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("conversation"))?;
    let messages = chat_repo::list_messages(&state.db, conversation_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({
        "conversation": conv,
        "messages": messages,
    })))
}

async fn delete_conversation(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let user = require_member(&state, &jar, workspace_id).await?;
    let ok = chat_repo::delete_conversation(&state.db, conversation_id, workspace_id, user.id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if !ok {
        return Err(AppError::not_found("conversation"));
    }
    Ok(Json(json!({ "deleted": true, "id": conversation_id })))
}

/// 404 when this user+workspace does not own `conversation_id` (missing or foreign).
async fn require_existing_conversation(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    conversation_id: Uuid,
) -> AppResult<()> {
    let exists = chat_repo::get_conversation(&state.db, conversation_id, workspace_id, user_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if exists.is_none() {
        return Err(AppError::not_found("conversation"));
    }
    Ok(())
}

/// Resume must 404 a foreign/missing `conversation_id` before plane-disabled 400.
/// New threads (`None`) skip this so a disabled provider cannot INSERT.
async fn require_conversation_if_resuming(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    conversation_id: Option<Uuid>,
) -> AppResult<()> {
    if let Some(cid) = conversation_id {
        require_existing_conversation(state, workspace_id, user_id, cid).await?;
    }
    Ok(())
}

/// Ensure conversation exists for this user; create if missing.
async fn ensure_conversation(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    conversation_id: Option<Uuid>,
    provider: &str,
    model: &str,
    first_user_text: &str,
) -> AppResult<Uuid> {
    if let Some(cid) = conversation_id {
        require_existing_conversation(state, workspace_id, user_id, cid).await?;
        return Ok(cid);
    }
    let id = Uuid::now_v7();
    let title = title_from_user_text(first_user_text);
    chat_repo::create_conversation(
        &state.db,
        id,
        workspace_id,
        user_id,
        &title,
        Some(provider),
        Some(model),
        Utc::now(),
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(id)
}

async fn persist_user_and_assistant(
    state: &AppState,
    conversation_id: Uuid,
    user_text: &str,
    assistant_text: &str,
    provider: &str,
    model: &str,
    actions: &[ChatAction],
) -> AppResult<()> {
    let now = Utc::now();
    let actions_json = serde_json::to_value(actions).unwrap_or_else(|_| json!([]));

    // Avoid duplicating the same trailing user turn on client retry.
    let existing = chat_repo::list_messages(&state.db, conversation_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let need_user = match existing.last() {
        Some(m) if m.role == "user" && m.content == user_text => false,
        Some(m) if m.role == "assistant" => true,
        None => true,
        Some(_) => true,
    };
    if need_user {
        chat_repo::insert_message(
            &state.db,
            Uuid::now_v7(),
            conversation_id,
            "user",
            user_text,
            Some(provider),
            Some(model),
            &json!([]),
            now,
        )
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    }

    chat_repo::insert_message(
        &state.db,
        Uuid::now_v7(),
        conversation_id,
        "assistant",
        assistant_text,
        Some(provider),
        Some(model),
        &actions_json,
        Utc::now(),
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    let title = if existing.is_empty() {
        Some(title_from_user_text(user_text))
    } else {
        None
    };
    chat_repo::touch_conversation(
        &state.db,
        conversation_id,
        title.as_deref(),
        Some(provider),
        Some(model),
        Utc::now(),
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(())
}

fn http_client(timeout: Duration) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| {
            tracing::error!(error = %e, "ollama http client build failed");
            AppError::internal(format!("http client: {e}"))
        })
}

/// Live model list from Ollama (`GET /api/tags` — same source as `ollama list`).
async fn fetch_models(cfg: &OllamaConfig) -> Result<Vec<ChatModelInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!("{}/api/tags", cfg.base_url);
    tracing::debug!(%url, "ollama list models (ollama list / /api/tags)");

    let res = client.get(&url).send().await.map_err(|e| {
        format!(
            "cannot reach Ollama at {}: {e}. Is `ollama serve` running?",
            cfg.base_url
        )
    })?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Ollama /api/tags returned {status}: {body}"));
    }

    let tags: TagsResponse = res
        .json()
        .await
        .map_err(|e| format!("Ollama /api/tags parse failed: {e}"))?;

    let mut models: Vec<ChatModelInfo> = tags
        .models
        .into_iter()
        .map(|m| ChatModelInfo {
            name: m.name,
            size: m.size,
            parameter_size: m.details.as_ref().and_then(|d| d.parameter_size.clone()),
            family: m.details.as_ref().and_then(|d| d.family.clone()),
        })
        .collect();

    // Stable order in the UI dropdown (A→Z by name).
    models.sort_by_key(|a| a.name.to_lowercase());
    Ok(models)
}

/// Incremental events for `POST …/chat/stream` (NDJSON).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatProgress {
    Status {
        text: String,
    },
    Delta {
        text: String,
    },
    Action {
        tool: String,
        ok: bool,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sandbox_ids: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<Value>,
    },
    Done {
        message: String,
        model: String,
        provider: String,
        conversation_id: Uuid,
        actions: Vec<ChatAction>,
    },
    Error {
        error: String,
    },
}

impl ChatProgress {
    pub fn from_action(a: &ChatAction) -> Self {
        Self::Action {
            tool: a.tool.clone(),
            ok: a.ok,
            summary: a.summary.clone(),
            run_id: a.run_id.clone(),
            sandbox_ids: a.sandbox_ids.clone(),
            detail: a.detail.clone(),
        }
    }
}

/// Ollama provider entry (available or offline with error).
fn ollama_provider(
    cfg: &OllamaConfig,
    models: Vec<ChatModelInfo>,
    error: Option<String>,
) -> ChatProviderInfo {
    ChatProviderInfo {
        id: "ollama".into(),
        name: "Ollama (local)".into(),
        available: error.is_none() && !models.is_empty(),
        coming_soon: false,
        error,
        default_model: cfg.default_model.clone(),
        models,
    }
}

/// One-shot text completion (no tools). Used by the case wake loop.
pub async fn complete_text(
    cfg: &OllamaConfig,
    model: &str,
    system: &str,
    user: &str,
) -> AppResult<String> {
    if !cfg.enabled {
        return Err(AppError::bad_request("Ollama is disabled"));
    }
    let model = model.trim();
    if model.is_empty() {
        return Err(AppError::bad_request("Ollama model is required"));
    }
    let messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": user }),
    ];
    let parsed = ollama_chat(cfg, model, &messages, false, None).await?;
    let content = parsed.message.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::bad_request("ollama returned empty decision"));
    }
    Ok(content)
}

/// One Ollama chat turn. When `with_tools` is true, sends tool definitions.
async fn emit_progress(
    progress: &Option<tokio::sync::mpsc::Sender<ChatProgress>>,
    ev: ChatProgress,
) {
    if let Some(tx) = progress {
        let _ = tx.send(ev).await;
    }
}

async fn ollama_chat(
    cfg: &OllamaConfig,
    model: &str,
    messages: &[Value],
    with_tools: bool,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<OllamaChatResponse> {
    if progress.is_some() {
        return ollama_chat_stream(cfg, model, messages, with_tools, progress).await;
    }
    crate::retry::retry_provider("ollama", || {
        ollama_chat_once(cfg, model, messages, with_tools)
    })
    .await
}

async fn ollama_chat_stream(
    cfg: &OllamaConfig,
    model: &str,
    messages: &[Value],
    with_tools: bool,
    progress: Option<&tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<OllamaChatResponse> {
    let client = http_client(cfg.timeout)?;
    let url = format!("{}/api/chat", cfg.base_url);
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    if with_tools {
        body["tools"] = tool_definitions();
    }
    let mut res = client.post(&url).json(&body).send().await.map_err(|e| {
        AppError::bad_request(format!(
            "cannot reach Ollama at {}: {e}. Start it with `ollama serve`.",
            cfg.base_url
        ))
    })?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(AppError::bad_request(format!(
            "Ollama error ({status}): {text}"
        )));
    }

    let mut buf = String::new();
    let mut content = String::new();
    let mut used_model = model.to_string();
    let mut tool_calls: Vec<OllamaToolCall> = Vec::new();
    let mut saw_tools = false;

    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| AppError::internal(format!("ollama stream chunk: {e}")))?
    {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find('\n') {
            let line = buf[..idx].trim().to_string();
            buf = buf[idx + 1..].to_string();
            if line.is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<OllamaChatResponse>(&line) else {
                continue;
            };
            if !parsed.model.is_empty() {
                used_model = parsed.model;
            }
            if !parsed.message.tool_calls.is_empty() {
                saw_tools = true;
                tool_calls = parsed.message.tool_calls;
            }
            let piece = parsed.message.content;
            if !piece.is_empty() {
                content.push_str(&piece);
                if !saw_tools {
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
    if !buf.trim().is_empty() {
        if let Ok(parsed) = serde_json::from_str::<OllamaChatResponse>(buf.trim()) {
            if !parsed.model.is_empty() {
                used_model = parsed.model;
            }
            if !parsed.message.tool_calls.is_empty() {
                saw_tools = true;
                tool_calls = parsed.message.tool_calls;
            }
            let piece = parsed.message.content;
            if !piece.is_empty() {
                content.push_str(&piece);
                if !saw_tools {
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

    Ok(OllamaChatResponse {
        model: used_model,
        message: OllamaMessage {
            content,
            tool_calls,
        },
    })
}

async fn ollama_chat_once(
    cfg: &OllamaConfig,
    model: &str,
    messages: &[Value],
    with_tools: bool,
) -> AppResult<OllamaChatResponse> {
    let client = http_client(cfg.timeout)?;
    let url = format!("{}/api/chat", cfg.base_url);

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
        "ollama chat request"
    );

    let res = client.post(&url).json(&body).send().await.map_err(|e| {
        tracing::error!(error = %e, %url, "ollama chat request failed");
        AppError::bad_request(format!(
            "cannot reach Ollama at {}: {e}. Start it with `ollama serve`.",
            cfg.base_url
        ))
    })?;

    let status = res.status();
    let text = res.text().await.map_err(|e| {
        tracing::error!(error = %e, "ollama chat body read failed");
        AppError::internal(format!("ollama response body: {e}"))
    })?;

    if !status.is_success() {
        tracing::error!(%status, body = %text, model = %model, "ollama chat error");
        // Common: model not found
        if text.contains("not found") || status.as_u16() == 404 {
            return Err(AppError::bad_request(format!(
                "model `{model}` not found in Ollama. Pull it with: ollama pull {model}"
            )));
        }
        // Some models reject tools — surface a clear message
        if text.to_lowercase().contains("tool") || text.to_lowercase().contains("does not support")
        {
            return Err(AppError::bad_request(format!(
                "model `{model}` may not support tool calling: {text}"
            )));
        }
        return Err(AppError::bad_request(format!(
            "Ollama error ({status}): {text}"
        )));
    }

    let parsed: OllamaChatResponse = serde_json::from_str(&text).map_err(|e| {
        tracing::error!(error = %e, body = %text, "ollama chat parse failed");
        AppError::internal(format!("ollama response parse: {e}"))
    })?;

    tracing::info!(
        model = %parsed.model,
        reply_chars = parsed.message.content.len(),
        tool_calls = parsed.message.tool_calls.len(),
        "ollama chat ok"
    );

    Ok(parsed)
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

/// Run Ollama with tools, execute tool_calls server-side, return final text + actions.
async fn chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &OllamaConfig,
    model: &str,
    mut history: Vec<Value>,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    // Inject live system prompt (integrations snapshot) as first message.
    let sys = system_prompt_for_workspace(state, workspace_id).await;
    if history
        .first()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        == Some("system")
    {
        history[0] = json!({ "role": "system", "content": sys });
    } else {
        history.insert(0, json!({ "role": "system", "content": sys }));
    }

    emit_progress(
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
        let parsed = ollama_chat(cfg, &used_model, &history, true, progress.as_ref()).await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model;
        }

        if parsed.message.tool_calls.is_empty() {
            let mut content = parsed.message.content.trim().to_string();
            if let Some(rewritten) = rewrite_if_asked_to_continue(&content, &actions) {
                content = rewritten;
            }
            if content.is_empty() && !actions.is_empty() {
                // Model returned only tool calls earlier; synthesize a short summary.
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok((used_model, format!("Done.\n{summary}"), actions));
            }
            return Ok((used_model, content, actions));
        }

        // Append assistant turn with tool_calls (Ollama expects this in history).
        let assistant_msg = json!({
            "role": "assistant",
            "content": parsed.message.content,
            "tool_calls": parsed.message.tool_calls,
        });
        history.push(assistant_msg);

        for (i, call) in parsed.message.tool_calls.iter().enumerate() {
            let name = call.function.name.trim();
            let args = tool_args_value(&call.function.arguments);
            tracing::info!(
                round,
                tool = %name,
                args = %args,
                "chat tool call"
            );
            let llm = ChatLlmContext {
                provider: "ollama".into(),
                model: model.to_string(),
                runner_id: None,
                effort: None,
            };
            let prior: Vec<Value> = actions.iter().filter_map(|a| a.detail.clone()).collect();
            emit_progress(
                &progress,
                ChatProgress::Status {
                    text: format!("Using {name}…"),
                },
            )
            .await;
            let (result_json, action) =
                execute_tool_with_retry(state, workspace_id, user_id, name, &args, &llm, &prior)
                    .await;
            emit_progress(&progress, ChatProgress::from_action(&action)).await;
            actions.push(action);

            let content = serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into());
            // Ollama tool result message
            let mut tool_msg = json!({
                "role": "tool",
                "content": content,
            });
            if let Some(id) = &call.id {
                tool_msg["tool_call_id"] = json!(id);
            } else {
                tool_msg["tool_call_id"] = json!(format!("call_{round}_{i}"));
            }
            tool_msg["name"] = json!(name);
            history.push(tool_msg);
        }
        let n = parsed.message.tool_calls.len();
        let failed: Vec<&ChatAction> = actions.iter().rev().take(n).filter(|a| !a.ok).collect();
        if !failed.is_empty() {
            history.push(json!({
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

    // Max rounds hit — one last call without tools for a user-facing summary.
    let parsed = ollama_chat(cfg, model, &history, false, progress.as_ref()).await?;
    used_model = parsed.model;
    let mut content = parsed.message.content.trim().to_string();
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

// ── handlers ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct ChatStatusQuery {
    #[serde(default)]
    runner_id: Option<Uuid>,
}

const STATUS_CATALOG: &[(&str, &str)] = &[
    ("grok", "Grok"),
    ("claude", "Claude"),
    ("openai", "OpenAI"),
    ("ollama", "Private (Ollama)"),
];

fn agent_status_phrase(id: &str, installed: bool, logged_in: bool) -> &'static str {
    let uses_login = id != "ollama";
    if !installed {
        "not installed"
    } else if uses_login && !logged_in {
        "installed · logged out"
    } else if uses_login {
        "installed · logged in"
    } else {
        "installed"
    }
}

fn providers_from_runner_agents(
    agents: &[WorkerAgentRuntime],
    openai_ready: bool,
) -> Vec<ChatProviderInfo> {
    STATUS_CATALOG
        .iter()
        .map(|(id, name)| {
            let reported = agents.iter().find(|a| a.id == *id);
            let installed = reported.map(|a| a.installed).unwrap_or(false);
            let logged_in = reported.map(|a| a.logged_in).unwrap_or(false);
            let models: Vec<ChatModelInfo> = reported
                .map(|a| {
                    a.models
                        .iter()
                        .map(|n| ChatModelInfo {
                            name: n.clone(),
                            size: None,
                            parameter_size: None,
                            family: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let phrase = agent_status_phrase(id, installed, logged_in);
            let available = if *id == "openai" {
                openai_ready
            } else if *id == "ollama" {
                installed && !models.is_empty()
            } else {
                installed && logged_in
            };
            ChatProviderInfo {
                id: (*id).into(),
                name: (*name).into(),
                available,
                coming_soon: false,
                error: if available && (*id == "ollama" || !models.is_empty()) {
                    None
                } else {
                    Some(phrase.to_string())
                },
                models,
                default_model: reported.and_then(|a| a.models.first().cloned()),
            }
        })
        .collect()
}

#[cfg(test)]
mod chat_tool_scope_tests {
    use super::*;
    use crate::chat_tools::require_session_workspace;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[test]
    fn chat_tool_args_preserve_workspace_id_and_foreign_is_forbidden() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let parsed = tool_args_value(&json!({
            "title": "watch",
            "policy_md": "wake",
            "workspace_id": foreign.to_string(),
        }));
        let err = require_session_workspace(session, &parsed)
            .expect_err("foreign workspace_id after tool_args_value");
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);

        let as_string = tool_args_value(&Value::String(format!(
            r#"{{"provider":"slack","operation":"list_channels","workspace_id":"{foreign}"}}"#
        )));
        let err = require_session_workspace(session, &as_string)
            .expect_err("foreign workspace_id in JSON string args");
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn chat_tool_session_member_cannot_target_another_workspace() {
        let session = Uuid::new_v4();
        let other = Uuid::new_v4();
        for name_args in [
            json!({ "title": "c", "policy_md": "p", "workspace_id": other }),
            json!({ "case_id": Uuid::new_v4(), "note": "n", "workspace_id": other }),
            json!({
                "provider": "jira",
                "operation": "search_issues",
                "workspace_id": other
            }),
        ] {
            let err = require_session_workspace(session, &tool_args_value(&name_args))
                .expect_err("session membership is not enough");
            assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
        }
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use fleet_db::WorkerAgentRuntime;

    #[test]
    fn claude_logged_in_without_models_is_not_logged_out() {
        let agents = vec![WorkerAgentRuntime {
            id: "claude".into(),
            name: Some("Claude".into()),
            installed: true,
            logged_in: true,
            models: vec![],
            model_details: vec![],
        }];
        let p = providers_from_runner_agents(&agents, false)
            .into_iter()
            .find(|p| p.id == "claude")
            .expect("claude");
        assert!(p.available);
        assert_ne!(p.error.as_deref().unwrap_or(""), "installed · logged out");
    }

    #[test]
    fn ollama_status_label_is_private() {
        let agents = vec![WorkerAgentRuntime {
            id: "ollama".into(),
            name: Some("Ollama".into()),
            installed: true,
            logged_in: true,
            models: vec!["llama3.2".into()],
            model_details: vec![],
        }];
        let p = providers_from_runner_agents(&agents, false)
            .into_iter()
            .find(|p| p.id == "ollama")
            .expect("ollama");
        assert_eq!(p.name, "Private (Ollama)");
        assert!(p.available);
    }

    #[test]
    fn public_base_url_strips_userinfo() {
        let mut cfg = OllamaConfig {
            base_url: "http://ollama-user:UNIQUEPASS@127.0.0.1:11434".into(),
            default_model: None,
            timeout: Duration::from_secs(1),
            enabled: true,
        };
        assert_eq!(cfg.public_base_url(), "http://127.0.0.1:11434");
        cfg.base_url = "https://token:x-oauth-basic@host.example/v1?q=1".into();
        assert_eq!(cfg.public_base_url(), "https://host.example/v1?q=1");
        cfg.base_url = "http://127.0.0.1:11434".into();
        assert_eq!(cfg.public_base_url(), "http://127.0.0.1:11434");
    }
}

/// `GET /api/workspaces/:id/chat/status`
///
/// Provider login/models come from the selected runner's last heartbeat.
/// The control plane is not a second inventory.
async fn chat_status(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<ChatStatusQuery>,
) -> AppResult<Json<ChatStatusResponse>> {
    let _user = require_member(&state, &jar, workspace_id).await?;

    let Some(runner_id) = q.runner_id else {
        let openai = crate::providers::openai_status(&state.openai);
        return Ok(Json(ChatStatusResponse {
            available: openai.available,
            enabled: true,
            base_url: state.ollama.public_base_url(),
            default_model: openai.default_model.clone(),
            models: openai.models.clone(),
            providers: vec![openai],
            error: None,
            hint: "Select a runner for Grok / Claude / Private (Ollama). OpenAI uses the control-plane API key.".into(),
        }));
    };

    let host = crate::worker_hosts::load_visible_host(&state, workspace_id, runner_id).await?;
    if crate::runner_chat::host_is_removed(&host) {
        return Ok(Json(ChatStatusResponse {
            available: false,
            enabled: true,
            base_url: state.ollama.public_base_url(),
            default_model: None,
            models: vec![],
            providers: vec![],
            error: Some(format!("runner `{}` is removed", host.name)),
            hint: format!("{} is removed and is not taking chat.", host.name),
        }));
    }
    let public = host.as_public(Utc::now());
    let mut providers = providers_from_runner_agents(&public.agents, state.openai.available());
    if let Some(p) = providers.iter_mut().find(|p| p.id == "openai") {
        *p = crate::providers::openai_status(&state.openai);
    }
    let any_available = providers.iter().any(|p| p.available);
    let ollama_models = providers
        .iter()
        .find(|p| p.id == "ollama")
        .map(|p| p.models.clone())
        .unwrap_or_default();
    let default_model = providers
        .iter()
        .find(|p| p.available)
        .and_then(|p| p.default_model.clone());
    let hint = format!("Providers are the last heartbeat from {}.", public.name);
    let error = if any_available {
        None
    } else {
        Some(format!("No ready provider on {}.", public.name))
    };

    Ok(Json(ChatStatusResponse {
        available: any_available,
        enabled: true,
        base_url: state.ollama.public_base_url(),
        default_model,
        models: ollama_models,
        providers,
        error,
        hint,
    }))
}

/// `POST /api/workspaces/:id/chat`
async fn chat_complete(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<ChatRequest>,
) -> AppResult<Json<ChatResponse>> {
    let user = require_member(&state, &jar, workspace_id).await?;

    let provider = crate::providers::parse_provider(body.provider.as_deref().unwrap_or("ollama"))?;

    if body.messages.is_empty() {
        return Err(AppError::bad_request("messages must not be empty"));
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    for m in &body.messages {
        let role = m.role.trim().to_lowercase();
        if role != "user" && role != "assistant" && role != "system" {
            return Err(AppError::bad_request(format!(
                "invalid message role `{role}` (use user|assistant|system)"
            )));
        }
        let content = m.content.trim();
        if content.is_empty() {
            continue;
        }
        if content.len() > 32_000 {
            return Err(AppError::bad_request(
                "message too long (max 32k bytes)",
            ));
        }
        // System messages from the client are folded into the first user turn context;
        // server always injects its own system prompt for tools.
        if role == "system" {
            pairs.push(("user".into(), format!("[context]\n{content}")));
        } else {
            pairs.push((role, content.to_string()));
        }
    }

    if pairs.is_empty() {
        return Err(AppError::bad_request(
            "no non-empty user/assistant messages",
        ));
    }

    let last_user = pairs
        .iter()
        .rev()
        .find(|(r, _)| r == "user")
        .map(|(_, c)| c.clone())
        .ok_or_else(|| AppError::bad_request("last turn must include a user message"))?;

    require_conversation_if_resuming(&state, workspace_id, user.id, body.conversation_id).await?;

    // Prefer DB history when resuming a conversation.
    // Cap turns sent to the model so a long thread cannot grow without bound.
    const MAX_LLM_HISTORY_TURNS: usize = 40;
    let mut pairs_for_llm = if let Some(cid) = body.conversation_id {
        let stored = chat_repo::list_messages(&state.db, cid)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        if stored.is_empty() {
            pairs.clone()
        } else {
            let mut hist: Vec<(String, String)> = stored
                .into_iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .map(|m| (m.role, m.content))
                .collect();
            match hist.last() {
                Some((r, c)) if r == "user" && c == &last_user => {}
                _ => hist.push(("user".into(), last_user.clone())),
            }
            hist
        }
    } else {
        pairs.clone()
    };
    if pairs_for_llm.len() > MAX_LLM_HISTORY_TURNS {
        pairs_for_llm = pairs_for_llm.split_off(pairs_for_llm.len() - MAX_LLM_HISTORY_TURNS);
    }

    require_plane_chat_enabled(&state, &provider)?;
    require_openai_configured(&state, &provider)?;

    if crate::runner_chat::runner_cli_provider(&provider) {
        let runner_id = body.runner_id.ok_or_else(|| {
            AppError::bad_request(
                "select a runner — Grok / Claude / Ollama chat runs on that box's official CLI"
                    .to_string(),
            )
        })?;
        let model = body
            .model
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let conversation_id = ensure_conversation(
            &state,
            workspace_id,
            user.id,
            body.conversation_id,
            &provider,
            &model,
            &last_user,
        )
        .await?;
        let (used_model, content) = crate::runner_chat::run_on_runner(
            &state,
            workspace_id,
            runner_id,
            &provider,
            &model,
            body.effort.as_deref(),
            &pairs_for_llm,
            None,
        )
        .await?;
        persist_user_and_assistant(
            &state,
            conversation_id,
            &last_user,
            &content,
            &provider,
            &used_model,
            &[],
        )
        .await?;
        return Ok(Json(ChatResponse {
            message: ChatMessageOut {
                role: "assistant".into(),
                content,
            },
            model: used_model,
            provider,
            actions: vec![],
            conversation_id,
        }));
    }

    match provider.as_str() {
        "grok" => {
            chat_complete_grok(
                &state,
                workspace_id,
                user.id,
                &body,
                pairs_for_llm,
                &last_user,
                None,
            )
            .await
        }
        "ollama" => {
            chat_complete_ollama(
                &state,
                workspace_id,
                user.id,
                &body,
                pairs_for_llm,
                &last_user,
                None,
            )
            .await
        }
        "openai" => {
            chat_complete_openai(
                &state,
                workspace_id,
                user.id,
                &body,
                pairs_for_llm,
                &last_user,
                None,
            )
            .await
        }
        "claude" => {
            chat_complete_claude(
                &state,
                workspace_id,
                user.id,
                &body,
                pairs_for_llm,
                &last_user,
                None,
            )
            .await
        }
        other => Err(AppError::bad_request(format!(
            "unknown provider `{other}` — use grok, ollama, openai, or claude"
        ))),
    }
}

async fn chat_stream(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<ChatRequest>,
) -> Result<axum::response::Response, AppError> {
    let user = require_member(&state, &jar, workspace_id).await?;
    if body.messages.is_empty() {
        return Err(AppError::bad_request("messages must not be empty"));
    }
    let provider = crate::providers::parse_provider(body.provider.as_deref().unwrap_or("ollama"))?;
    // Resume: 404 foreign/missing conversation_id before plane-disabled 400.
    // New thread: plane flags / missing OPENAI_API_KEY 400 before NDJSON / INSERT.
    require_conversation_if_resuming(&state, workspace_id, user.id, body.conversation_id).await?;
    require_plane_chat_enabled(&state, &provider)?;
    require_openai_configured(&state, &provider)?;
    let prepared = prepare_chat_pairs(&body)?;
    // Gate before NDJSON so a revoked leftover runner_id is HTTP 400, not
    // 200 + type=error after enqueue / the 240s wait starts.
    if crate::runner_chat::runner_cli_provider(&provider) {
        if let Some(runner_id) = body.runner_id {
            crate::runner_chat::require_taking(&state, workspace_id, runner_id).await?;
        }
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<ChatProgress>(64);
    let state = state.clone();
    tokio::spawn(async move {
        let result = run_chat_turn(
            &state,
            workspace_id,
            user.id,
            &body,
            &provider,
            prepared.pairs,
            &prepared.last_user,
            Some(tx.clone()),
        )
        .await;
        match result {
            Ok(done) => {
                let _ = tx.send(done).await;
            }
            Err(e) => {
                let _ = tx
                    .send(ChatProgress::Error {
                        error: e.to_string(),
                    })
                    .await;
            }
        }
    });

    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            let line = serde_json::to_string(&ev).unwrap_or_else(|_| {
                r#"{"type":"error","error":"encode"}"#.into()
            });
            yield Ok::<_, std::convert::Infallible>(format!("{line}\n"));
        }
    };

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/x-ndjson"),
            ),
            (
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-cache"),
            ),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

struct PreparedChat {
    pairs: Vec<(String, String)>,
    last_user: String,
}

/// GROK_ENABLED=false / OLLAMA_ENABLED=false must 400 before runner_id or INSERT.
fn require_plane_chat_enabled(state: &AppState, provider: &str) -> AppResult<()> {
    match provider {
        "grok" if !state.grok.enabled => Err(AppError::bad_request(
            "Grok chat is disabled (GROK_ENABLED=false)",
        )),
        "ollama" if !state.ollama.enabled => Err(AppError::bad_request(
            "Ollama chat is disabled (OLLAMA_ENABLED=false)",
        )),
        _ => Ok(()),
    }
}

/// Missing OPENAI_API_KEY must 400 before runner_id / ensure_conversation.
/// Claude is runner CLI — do not require a plane ANTHROPIC_API_KEY here.
fn require_openai_configured(state: &AppState, provider: &str) -> AppResult<()> {
    if provider == "openai" && !state.openai.available() {
        return Err(AppError::bad_request(
            "OpenAI is not configured (set OPENAI_API_KEY)",
        ));
    }
    Ok(())
}

fn prepare_chat_pairs(body: &ChatRequest) -> AppResult<PreparedChat> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for m in &body.messages {
        let role = m.role.trim().to_lowercase();
        if role != "user" && role != "assistant" && role != "system" {
            return Err(AppError::bad_request(format!(
                "invalid message role `{role}` (use user|assistant|system)"
            )));
        }
        let content = m.content.trim();
        if content.is_empty() {
            continue;
        }
        if content.len() > 32_000 {
            return Err(AppError::bad_request(
                "message too long (max 32k bytes)",
            ));
        }
        if role == "system" {
            pairs.push(("user".into(), format!("[context]\n{content}")));
        } else {
            pairs.push((role, content.to_string()));
        }
    }
    if pairs.is_empty() {
        return Err(AppError::bad_request(
            "no non-empty user/assistant messages",
        ));
    }
    let last_user = pairs
        .iter()
        .rev()
        .find(|(r, _)| r == "user")
        .map(|(_, c)| c.clone())
        .ok_or_else(|| AppError::bad_request("last turn must include a user message"))?;
    Ok(PreparedChat { pairs, last_user })
}

async fn run_chat_turn(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    body: &ChatRequest,
    provider: &str,
    pairs_in: Vec<(String, String)>,
    last_user: &str,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<ChatProgress> {
    let provider = crate::providers::parse_provider(provider)?;
    require_conversation_if_resuming(state, workspace_id, user_id, body.conversation_id).await?;
    const MAX_LLM_HISTORY_TURNS: usize = 40;
    let mut pairs_for_llm = if let Some(cid) = body.conversation_id {
        let stored = chat_repo::list_messages(&state.db, cid)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        if stored.is_empty() {
            pairs_in.clone()
        } else {
            let mut hist: Vec<(String, String)> = stored
                .into_iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .map(|m| (m.role, m.content))
                .collect();
            match hist.last() {
                Some((r, c)) if r == "user" && c == last_user => {}
                _ => hist.push(("user".into(), last_user.to_string())),
            }
            hist
        }
    } else {
        pairs_in
    };
    if pairs_for_llm.len() > MAX_LLM_HISTORY_TURNS {
        pairs_for_llm = pairs_for_llm.split_off(pairs_for_llm.len() - MAX_LLM_HISTORY_TURNS);
    }

    require_plane_chat_enabled(state, &provider)?;
    require_openai_configured(state, &provider)?;

    if crate::runner_chat::runner_cli_provider(&provider) {
        let runner_id = body.runner_id.ok_or_else(|| {
            AppError::bad_request(
                "select a runner — Grok / Claude / Ollama chat runs on that box's official CLI"
                    .to_string(),
            )
        })?;
        let model = body
            .model
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let conversation_id = ensure_conversation(
            state,
            workspace_id,
            user_id,
            body.conversation_id,
            &provider,
            &model,
            last_user,
        )
        .await?;
        let (used_model, content) = crate::runner_chat::run_on_runner(
            state,
            workspace_id,
            runner_id,
            &provider,
            &model,
            body.effort.as_deref(),
            &pairs_for_llm,
            progress.as_ref(),
        )
        .await?;
        persist_user_and_assistant(
            state,
            conversation_id,
            last_user,
            &content,
            &provider,
            &used_model,
            &[],
        )
        .await?;
        return Ok(ChatProgress::Done {
            message: content,
            model: used_model,
            provider,
            conversation_id,
            actions: vec![],
        });
    }

    match provider.as_str() {
        "grok" => {
            let Json(res) = chat_complete_grok(
                state,
                workspace_id,
                user_id,
                body,
                pairs_for_llm,
                last_user,
                progress,
            )
            .await?;
            return Ok(ChatProgress::Done {
                message: res.message.content,
                model: res.model,
                provider: res.provider,
                conversation_id: res.conversation_id,
                actions: res.actions,
            });
        }
        "ollama" => {
            let Json(res) = chat_complete_ollama(
                state,
                workspace_id,
                user_id,
                body,
                pairs_for_llm,
                last_user,
                progress,
            )
            .await?;
            return Ok(ChatProgress::Done {
                message: res.message.content,
                model: res.model,
                provider: res.provider,
                conversation_id: res.conversation_id,
                actions: res.actions,
            });
        }
        "openai" => {
            let Json(res) = chat_complete_openai(
                state,
                workspace_id,
                user_id,
                body,
                pairs_for_llm,
                last_user,
                progress,
            )
            .await?;
            return Ok(ChatProgress::Done {
                message: res.message.content,
                model: res.model,
                provider: res.provider,
                conversation_id: res.conversation_id,
                actions: res.actions,
            });
        }
        "claude" => {
            let Json(res) = chat_complete_claude(
                state,
                workspace_id,
                user_id,
                body,
                pairs_for_llm,
                last_user,
                progress,
            )
            .await?;
            return Ok(ChatProgress::Done {
                message: res.message.content,
                model: res.model,
                provider: res.provider,
                conversation_id: res.conversation_id,
                actions: res.actions,
            });
        }
        other => Err(AppError::bad_request(format!(
            "unknown provider `{other}` — use grok, ollama, openai, or claude"
        ))),
    }
}

async fn chat_complete_grok(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    body: &ChatRequest,
    pairs: Vec<(String, String)>,
    last_user: &str,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<Json<ChatResponse>> {
    require_plane_chat_enabled(state, "grok")?;
    let cfg = &state.grok;
    let model = body
        .model
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(crate::grok::default_model);

    let conversation_id = ensure_conversation(
        state,
        workspace_id,
        user_id,
        body.conversation_id,
        "grok",
        &model,
        last_user,
    )
    .await?;

    tracing::info!(
        provider = "grok",
        model = %model,
        %conversation_id,
        "chat complete (CLI auth + tools)"
    );

    let (used_model, content, actions) = crate::grok::chat_with_tools(
        state,
        workspace_id,
        user_id,
        cfg,
        &model,
        &pairs,
        progress,
        body.runner_id,
        body.effort.clone(),
    )
    .await?;

    persist_user_and_assistant(
        state,
        conversation_id,
        last_user,
        &content,
        "grok",
        &used_model,
        &actions,
    )
    .await?;

    Ok(Json(ChatResponse {
        message: ChatMessageOut {
            role: "assistant".into(),
            content,
        },
        model: used_model,
        provider: "grok".into(),
        actions,
        conversation_id,
    }))
}

async fn chat_complete_ollama(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    body: &ChatRequest,
    pairs: Vec<(String, String)>,
    last_user: &str,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<Json<ChatResponse>> {
    let cfg = &state.ollama;
    if !cfg.enabled {
        return Err(AppError::bad_request(
            "Ollama chat is disabled (set OLLAMA_ENABLED=true)",
        ));
    }

    let mut history: Vec<Value> = Vec::with_capacity(pairs.len() + 1);
    // Placeholder system — replaced inside chat_with_tools with live integrations.
    history.push(json!({
        "role": "system",
        "content": system_prompt_with_tools(),
    }));
    for (role, content) in &pairs {
        history.push(json!({ "role": role, "content": content }));
    }

    let model = if let Some(m) = body
        .model
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        m
    } else if let Some(m) = cfg.default_model.clone() {
        m
    } else {
        let models = fetch_models(cfg).await.map_err(|e| {
            tracing::error!(error = %e, "ollama models for default pick failed");
            AppError::bad_request(e)
        })?;
        models.first().map(|m| m.name.clone()).ok_or_else(|| {
            AppError::bad_request(
                "no models installed in Ollama. Run `ollama pull` on the runner.".to_string(),
            )
        })?
    };

    let conversation_id = ensure_conversation(
        state,
        workspace_id,
        user_id,
        body.conversation_id,
        "ollama",
        &model,
        last_user,
    )
    .await?;

    tracing::info!(
        provider = "ollama",
        model = %model,
        %conversation_id,
        "chat complete with selected model + tools"
    );

    let (used_model, content, actions) =
        chat_with_tools(state, workspace_id, user_id, cfg, &model, history, progress).await?;

    persist_user_and_assistant(
        state,
        conversation_id,
        last_user,
        &content,
        "ollama",
        &used_model,
        &actions,
    )
    .await?;

    Ok(Json(ChatResponse {
        message: ChatMessageOut {
            role: "assistant".into(),
            content,
        },
        model: used_model,
        provider: "ollama".into(),
        actions,
        conversation_id,
    }))
}

async fn chat_complete_openai(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    body: &ChatRequest,
    pairs: Vec<(String, String)>,
    last_user: &str,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<Json<ChatResponse>> {
    require_openai_configured(state, "openai")?;
    let model = body
        .model
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let conversation_id = ensure_conversation(
        state,
        workspace_id,
        user_id,
        body.conversation_id,
        "openai",
        &model,
        last_user,
    )
    .await?;
    let (used_model, content, actions) = crate::providers::openai_chat_with_tools(
        state,
        workspace_id,
        user_id,
        &state.openai,
        &model,
        &pairs,
        progress,
        body.runner_id,
    )
    .await?;
    persist_user_and_assistant(
        state,
        conversation_id,
        last_user,
        &content,
        "openai",
        &used_model,
        &actions,
    )
    .await?;
    Ok(Json(ChatResponse {
        message: ChatMessageOut {
            role: "assistant".into(),
            content,
        },
        model: used_model,
        provider: "openai".into(),
        actions,
        conversation_id,
    }))
}

async fn chat_complete_claude(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    body: &ChatRequest,
    pairs: Vec<(String, String)>,
    last_user: &str,
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
) -> AppResult<Json<ChatResponse>> {
    let model = body
        .model
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let conversation_id = ensure_conversation(
        state,
        workspace_id,
        user_id,
        body.conversation_id,
        "claude",
        &model,
        last_user,
    )
    .await?;
    let (used_model, content, actions) = crate::providers::claude_chat_with_tools(
        state,
        workspace_id,
        user_id,
        &state.claude,
        &model,
        &pairs,
        progress,
        body.runner_id,
    )
    .await?;
    persist_user_and_assistant(
        state,
        conversation_id,
        last_user,
        &content,
        "claude",
        &used_model,
        &actions,
    )
    .await?;
    Ok(Json(ChatResponse {
        message: ChatMessageOut {
            role: "assistant".into(),
            content,
        },
        model: used_model,
        provider: "claude".into(),
        actions,
        conversation_id,
    }))
}
