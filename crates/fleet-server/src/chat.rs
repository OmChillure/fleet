//! Workspace chat backed by a local [Ollama](https://ollama.com) instance.
//!
//! | Method | Path | Role |
//! | --- | --- | --- |
//! | `GET` | `/api/workspaces/:id/chat/status` | Ollama reachability + models |
//! | `POST` | `/api/workspaces/:id/chat` | Send message history, get assistant reply |

use std::env;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::workspaces as workspace_repo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

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
        .route("/api/workspaces/{workspace_id}/chat", post(chat_complete))
}

// ── request / response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// Full conversation so far (user + assistant turns). Last message should be user.
    pub messages: Vec<ChatMessageIn>,
    /// Provider id (`ollama` today; more later).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model name as returned by the provider list (e.g. `llama3.2:latest`).
    #[serde(default)]
    pub model: Option<String>,
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
}

#[derive(Debug, Serialize)]
pub struct ChatMessageOut {
    pub role: String,
    pub content: String,
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
    #[allow(dead_code)]
    role: String,
    content: String,
}

const SYSTEM_PROMPT: &str = "You are the Agent Fleet assistant — a helpful AI embedded in a developer platform that runs fleets of sandboxed coding agents and connects to tools (GitHub, Jira, Slack, Linear, …).\n\nHelp the user plan work, reason about agent runs, draft task specs, and use workspace integrations. Be concise and practical. If you are unsure about live fleet state, say so rather than inventing IDs or statuses.";

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<()> {
    let auth = AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    };
    let user = require_user(&auth, jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    tracing::debug!(%workspace_id, user_id = %user.id, %role, "chat: workspace member ok");
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
    models.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(models)
}

/// Placeholder providers the UI can show as “coming soon”.
fn future_providers() -> Vec<ChatProviderInfo> {
    vec![
        ChatProviderInfo {
            id: "xai".into(),
            name: "SpaceXAI (xAI)".into(),
            available: false,
            coming_soon: true,
            error: None,
            models: vec![],
            default_model: None,
        },
        ChatProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            available: false,
            coming_soon: true,
            error: None,
            models: vec![],
            default_model: None,
        },
        ChatProviderInfo {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            available: false,
            coming_soon: true,
            error: None,
            models: vec![],
            default_model: None,
        },
    ]
}

async fn ollama_chat(
    cfg: &OllamaConfig,
    model: &str,
    messages: Vec<Value>,
) -> AppResult<(String, String)> {
    let client = http_client(cfg.timeout)?;
    let url = format!("{}/api/chat", cfg.base_url);

    let body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    tracing::info!(model = %model, n_messages = messages.len(), "ollama chat request");

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
        "ollama chat ok"
    );

    Ok((parsed.model, parsed.message.content))
}

// ── handlers ────────────────────────────────────────────────────────────────

/// `GET /api/workspaces/:id/chat/status`
async fn chat_status(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<ChatStatusResponse>> {
    require_member(&state, &jar, workspace_id).await?;

    let cfg = &state.ollama;
    let hint = format!(
        "Install Ollama from https://ollama.com , then: ollama serve && ollama pull {}",
        cfg.default_model.as_deref().unwrap_or("llama3.2")
    );

    let mut future = future_providers();

    if !cfg.enabled {
        let ollama = ChatProviderInfo {
            id: "ollama".into(),
            name: "Ollama (local)".into(),
            available: false,
            coming_soon: false,
            error: Some("Ollama chat is disabled (OLLAMA_ENABLED=false)".into()),
            models: vec![],
            default_model: cfg.default_model.clone(),
        };
        let mut providers = vec![ollama];
        providers.append(&mut future);
        return Ok(Json(ChatStatusResponse {
            available: false,
            enabled: false,
            base_url: cfg.base_url.clone(),
            default_model: cfg.default_model.clone(),
            models: vec![],
            providers,
            error: Some("Ollama chat is disabled (OLLAMA_ENABLED=false)".into()),
            hint,
        }));
    }

    match fetch_models(cfg).await {
        Ok(models) => {
            tracing::info!(
                count = models.len(),
                models = ?models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                "ollama models listed (live from /api/tags)"
            );
            let ollama = ChatProviderInfo {
                id: "ollama".into(),
                name: "Ollama (local)".into(),
                available: true,
                coming_soon: false,
                error: None,
                default_model: cfg.default_model.clone(),
                models: models.clone(),
            };
            let mut providers = vec![ollama];
            providers.append(&mut future);
            Ok(Json(ChatStatusResponse {
                available: true,
                enabled: true,
                base_url: cfg.base_url.clone(),
                default_model: cfg.default_model.clone(),
                models,
                providers,
                error: None,
                hint,
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "ollama unavailable");
            let ollama = ChatProviderInfo {
                id: "ollama".into(),
                name: "Ollama (local)".into(),
                available: false,
                coming_soon: false,
                error: Some(e.clone()),
                models: vec![],
                default_model: cfg.default_model.clone(),
            };
            let mut providers = vec![ollama];
            providers.append(&mut future);
            Ok(Json(ChatStatusResponse {
                available: false,
                enabled: true,
                base_url: cfg.base_url.clone(),
                default_model: cfg.default_model.clone(),
                models: vec![],
                providers,
                error: Some(e),
                hint,
            }))
        }
    }
}

/// `POST /api/workspaces/:id/chat`
async fn chat_complete(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<ChatRequest>,
) -> AppResult<Json<ChatResponse>> {
    require_member(&state, &jar, workspace_id).await?;

    let provider = body
        .provider
        .as_deref()
        .unwrap_or("ollama")
        .trim()
        .to_lowercase();

    if provider != "ollama" {
        return Err(AppError::bad_request(format!(
            "provider `{provider}` is not available yet — use Ollama (local)"
        )));
    }

    let cfg = &state.ollama;
    if !cfg.enabled {
        return Err(AppError::bad_request(
            "Ollama chat is disabled (set OLLAMA_ENABLED=true)",
        ));
    }

    if body.messages.is_empty() {
        return Err(AppError::bad_request("messages must not be empty"));
    }

    // Validate roles / trim empty content
    let mut history: Vec<Value> = Vec::with_capacity(body.messages.len() + 1);
    history.push(json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    }));

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
        history.push(json!({ "role": role, "content": content }));
    }

    if history.len() < 2 {
        return Err(AppError::bad_request("no non-empty user/assistant messages"));
    }

    // Exact model name from the UI dropdown (matches `ollama list`); else default / first.
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

    tracing::info!(%provider, model = %model, "chat complete with selected model");

    let (used_model, content) = ollama_chat(cfg, &model, history).await?;

    Ok(Json(ChatResponse {
        message: ChatMessageOut {
            role: "assistant".into(),
            content,
        },
        model: used_model,
        provider,
    }))
}
