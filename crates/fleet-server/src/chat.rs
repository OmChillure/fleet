//! Workspace chat — multi-provider (Grok / xAI + local Ollama).
//!
//! | Method | Path | Role |
//! | --- | --- | --- |
//! | `GET` | `/api/workspaces/:id/chat/status` | Providers + model lists |
//! | `GET` | `/api/workspaces/:id/chat/conversations` | List saved chats |
//! | `POST` | `/api/workspaces/:id/chat/conversations` | Create empty chat |
//! | `GET` | `/api/workspaces/:id/chat/conversations/:cid` | Load chat + messages |
//! | `DELETE` | `/api/workspaces/:id/chat/conversations/:cid` | Delete chat |
//! | `POST` | `/api/workspaces/:id/chat` | Complete turn (persist + tools) |
//!
//! See [`crate::chat_tools`] and [`crate::grok`].

use std::env;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::{chats as chat_repo, workspaces as workspace_repo};
use fleet_db::User;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chat_tools::{
    execute_tool, system_prompt_for_workspace, system_prompt_with_tools, tool_definitions,
    ChatAction, MAX_TOOL_ROUNDS,
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

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<User> {
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
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("New chat");
    let id = Uuid::now_v7();
    let now = Utc::now();
    let conv = chat_repo::create_conversation(
        &state.db,
        id,
        workspace_id,
        user.id,
        title,
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
        let exists =
            chat_repo::get_conversation(&state.db, cid, workspace_id, user_id)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
        if exists.is_none() {
            return Err(AppError::not_found("conversation"));
        }
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

/// Placeholder providers the UI can show as “coming soon”.
/// Grok is a real provider (`crate::grok`) — not listed here.
fn future_providers() -> Vec<ChatProviderInfo> {
    vec![ChatProviderInfo {
        id: "openai".into(),
        name: "OpenAI".into(),
        available: false,
        coming_soon: true,
        error: None,
        models: vec![],
        default_model: None,
    }]
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

/// One Ollama chat turn. When `with_tools` is true, sends tool definitions.
async fn ollama_chat(
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

    let res = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
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

    let mut actions: Vec<ChatAction> = Vec::new();
    let mut used_model = model.to_string();

    for round in 0..MAX_TOOL_ROUNDS {
        let parsed = ollama_chat(cfg, &used_model, &history, true).await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model;
        }

        if parsed.message.tool_calls.is_empty() {
            let content = parsed.message.content.trim().to_string();
            if content.is_empty() && !actions.is_empty() {
                // Model returned only tool calls earlier; synthesize a short summary.
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok((
                    used_model,
                    format!("Done.\n{summary}"),
                    actions,
                ));
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
            let (result_json, action) =
                execute_tool(state, workspace_id, user_id, name, &args).await;
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
    }

    // Max rounds hit — one last call without tools for a user-facing summary.
    let parsed = ollama_chat(cfg, model, &history, false).await?;
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

/// `GET /api/workspaces/:id/chat/status`
async fn chat_status(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<ChatStatusResponse>> {
    let _user = require_member(&state, &jar, workspace_id).await?;

    let cfg = &state.ollama;
    let grok = crate::grok::provider_status(&state.grok).await;

    // Ollama entry
    let (ollama, ollama_models, ollama_err) = if !cfg.enabled {
        (
            ollama_provider(
                cfg,
                vec![],
                Some("Ollama chat is disabled (OLLAMA_ENABLED=false)".into()),
            ),
            vec![],
            Some("Ollama chat is disabled (OLLAMA_ENABLED=false)".into()),
        )
    } else {
        match fetch_models(cfg).await {
            Ok(models) => {
                tracing::info!(
                    count = models.len(),
                    models = ?models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                    "ollama models listed (live from /api/tags)"
                );
                let err = if models.is_empty() {
                    Some(
                        "No models in Ollama. Run: ollama pull llama3.2".to_string(),
                    )
                } else {
                    None
                };
                let p = ollama_provider(cfg, models.clone(), err.clone());
                (p, models, err)
            }
            Err(e) => {
                tracing::warn!(error = %e, "ollama unavailable");
                (
                    ollama_provider(cfg, vec![], Some(e.clone())),
                    vec![],
                    Some(e),
                )
            }
        }
    };

    // Prefer Grok first when local CLI auth is present.
    let mut providers = Vec::new();
    if grok.available {
        providers.push(grok.clone());
        providers.push(ollama);
    } else {
        providers.push(ollama);
        providers.push(grok.clone());
    }
    providers.extend(future_providers());

    let any_available = providers.iter().any(|p| p.available);
    let hint = if grok.available {
        "Grok is ready (local CLI auth). Model: grok-4.5 — or use Ollama locally.".to_string()
    } else if ollama_err.is_none() && !ollama_models.is_empty() {
        "Ollama is ready. For Grok: run `grok login` (uses ~/.grok/auth.json).".to_string()
    } else {
        format!(
            "Start Ollama (`ollama serve && ollama pull {}`) and/or run `grok login` for Grok.",
            cfg.default_model.as_deref().unwrap_or("llama3.2")
        )
    };

    // Top-level error only when nothing works.
    let error = if any_available {
        None
    } else {
        let parts: Vec<String> = providers
            .iter()
            .filter(|p| !p.coming_soon)
            .filter_map(|p| p.error.clone())
            .collect();
        if parts.is_empty() {
            Some("No chat providers available".into())
        } else {
            Some(parts.join(" · "))
        }
    };

    Ok(Json(ChatStatusResponse {
        available: any_available,
        enabled: cfg.enabled || state.grok.enabled,
        base_url: cfg.base_url.clone(),
        default_model: grok
            .default_model
            .clone()
            .or_else(|| cfg.default_model.clone()),
        // Flat list keeps older clients on Ollama models.
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

    let provider = body
        .provider
        .as_deref()
        .unwrap_or("ollama")
        .trim()
        .to_lowercase();

    if body.messages.is_empty() {
        return Err(AppError::bad_request("messages must not be empty"));
    }

    // Validate roles / trim empty content (shared).
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
                "message too long (max 32k characters)",
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
        return Err(AppError::bad_request("no non-empty user/assistant messages"));
    }

    let last_user = pairs
        .iter()
        .rev()
        .find(|(r, _)| r == "user")
        .map(|(_, c)| c.clone())
        .ok_or_else(|| AppError::bad_request("last turn must include a user message"))?;

    // Prefer DB history when resuming a conversation.
    let pairs_for_llm = if let Some(cid) = body.conversation_id {
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
            // Append new user message if not already last.
            match hist.last() {
                Some((r, c)) if r == "user" && c == &last_user => {}
                _ => hist.push(("user".into(), last_user.clone())),
            }
            hist
        }
    } else {
        pairs.clone()
    };

    match provider.as_str() {
        "grok" | "xai" => {
            chat_complete_grok(
                &state,
                workspace_id,
                user.id,
                &body,
                pairs_for_llm,
                &last_user,
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
            )
            .await
        }
        other => Err(AppError::bad_request(format!(
            "provider `{other}` is not available — use `grok` or `ollama`"
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
) -> AppResult<Json<ChatResponse>> {
    let cfg = &state.grok;
    let model = body
        .model
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::grok::GROK_MODEL.to_string());

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

    let (used_model, content, actions) =
        crate::grok::chat_with_tools(state, workspace_id, user_id, cfg, &model, &pairs).await?;

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
        models
            .first()
            .map(|m| m.name.clone())
            .ok_or_else(|| {
                AppError::bad_request(
                    "no models installed in Ollama. Run: ollama pull llama3.2".to_string(),
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
        chat_with_tools(state, workspace_id, user_id, cfg, &model, history).await?;

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
