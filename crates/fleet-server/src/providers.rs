//! OpenAI and Anthropic (Claude) chat providers — same tool loop as Grok.

use std::env;
use std::time::Duration;

use fleet_auth::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chat::{ChatModelInfo, ChatProgress, ChatProviderInfo};
use crate::chat_tools::{
    execute_tool_with_retry, rewrite_if_asked_to_continue, system_prompt_for_workspace,
    tool_definitions, ChatAction, ChatLlmContext, MAX_TOOL_ROUNDS,
};
use crate::state::AppState;

const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
const GEMINI_DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

fn openai_default_model(models: &[ChatModelInfo]) -> Option<String> {
    models.first().map(|m| m.name.clone())
}

fn claude_default_model(models: &[ChatModelInfo]) -> Option<String> {
    models.first().map(|m| m.name.clone())
}

#[derive(Clone, Debug)]
pub struct OpenaiConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub api_base: String,
}

impl OpenaiConfig {
    pub fn from_env() -> Self {
        let enabled = env_flag("OPENAI_ENABLED", true);
        let api_key = env_opt("OPENAI_API_KEY");
        let api_base = env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| OPENAI_DEFAULT_BASE.into())
            .trim_end_matches('/')
            .to_string();
        Self {
            enabled,
            api_key,
            api_base,
        }
    }

    pub fn available(&self) -> bool {
        self.enabled && self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub api_base: String,
}

#[derive(Clone, Debug)]
pub struct GeminiConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub api_base: String,
}

impl GeminiConfig {
    pub fn from_env() -> Self {
        let enabled = env_flag("GEMINI_ENABLED", true);
        let api_key = env_opt("GEMINI_API_KEY").or_else(|| env_opt("GOOGLE_API_KEY"));
        let api_base = env::var("GEMINI_API_BASE")
            .unwrap_or_else(|_| GEMINI_DEFAULT_BASE.into())
            .trim_end_matches('/')
            .to_string();
        Self {
            enabled,
            api_key,
            api_base,
        }
    }

    pub fn available(&self) -> bool {
        self.enabled && self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }

    fn as_openai(&self) -> OpenaiConfig {
        OpenaiConfig {
            enabled: self.enabled,
            api_key: self.api_key.clone(),
            api_base: self.api_base.clone(),
        }
    }
}

impl ClaudeConfig {
    pub fn from_env() -> Self {
        let enabled = env_flag("ANTHROPIC_ENABLED", true) && env_flag("CLAUDE_ENABLED", true);
        let api_key = env_opt("ANTHROPIC_API_KEY").or_else(|| env_opt("CLAUDE_API_KEY"));
        let api_base = env::var("ANTHROPIC_API_BASE")
            .unwrap_or_else(|_| ANTHROPIC_DEFAULT_BASE.into())
            .trim_end_matches('/')
            .to_string();
        Self {
            enabled,
            api_key,
            api_base,
        }
    }

    pub fn available(&self) -> bool {
        self.enabled && self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|v| {
            let v = v.trim().to_lowercase();
            !(v == "0" || v == "false" || v == "no" || v == "off")
        })
        .unwrap_or(default)
}

fn env_opt(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn openai_models() -> Vec<ChatModelInfo> {
    split_models(env::var("OPENAI_MODELS").ok().as_deref(), "openai")
}

fn claude_models() -> Vec<ChatModelInfo> {
    split_models(
        env::var("ANTHROPIC_MODELS")
            .ok()
            .or_else(|| env::var("CLAUDE_MODELS").ok())
            .as_deref(),
        "claude",
    )
}

fn split_models(raw: Option<&str>, family: &str) -> Vec<ChatModelInfo> {
    let names: Vec<String> = raw
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    names
        .into_iter()
        .map(|name| ChatModelInfo {
            name,
            size: None,
            parameter_size: None,
            family: Some(family.into()),
        })
        .collect()
}

/// Canonical chat / `TASK_PROVIDER` id. Empty, typo, and ticket names fail closed.
pub fn parse_provider(raw: &str) -> AppResult<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok("openai".into()),
        "gemini" | "google" => Ok("gemini".into()),
        "claude" | "anthropic" => Ok("claude".into()),
        "grok" | "xai" => Ok("grok".into()),
        "ollama" => Ok("ollama".into()),
        other => Err(AppError::bad_request(format!(
            "unknown provider `{other}` — use grok, ollama, openai, gemini, or claude"
        ))),
    }
}

pub fn openai_status(cfg: &OpenaiConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            available: false,
            coming_soon: false,
            error: Some("OpenAI is disabled".into()),
            models: vec![],
            default_model: None,
        };
    }
    if !cfg.available() {
        let models = openai_models();
        let default_model = openai_default_model(&models);
        return ChatProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            available: false,
            coming_soon: false,
            error: Some(
                "Set OPENAI_API_KEY or pick a runner with Codex / the OpenAI CLI.".into(),
            ),
            models,
            default_model,
        };
    }
    let models = openai_models();
    let default_model = openai_default_model(&models);
    ChatProviderInfo {
        id: "openai".into(),
        name: "OpenAI".into(),
        available: true,
        coming_soon: false,
        error: None,
        models,
        default_model,
    }
}

fn gemini_models() -> Vec<ChatModelInfo> {
    split_models(env::var("GEMINI_MODELS").ok().as_deref(), "gemini")
}

fn gemini_default_model(models: &[ChatModelInfo]) -> Option<String> {
    models.first().map(|m| m.name.clone()).or_else(|| {
        Some("gemini-2.5-flash".into())
    })
}

pub fn gemini_status(cfg: &GeminiConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "gemini".into(),
            name: "Gemini".into(),
            available: false,
            coming_soon: false,
            error: Some("Gemini is disabled".into()),
            models: vec![],
            default_model: None,
        };
    }
    if !cfg.available() {
        let models = gemini_models();
        let default_model = gemini_default_model(&models);
        return ChatProviderInfo {
            id: "gemini".into(),
            name: "Gemini".into(),
            available: false,
            coming_soon: false,
            error: Some("Set GEMINI_API_KEY or pick a runner with the Gemini CLI.".into()),
            models,
            default_model,
        };
    }
    let models = gemini_models();
    let default_model = gemini_default_model(&models);
    ChatProviderInfo {
        id: "gemini".into(),
        name: "Gemini".into(),
        available: true,
        coming_soon: false,
        error: None,
        models,
        default_model,
    }
}

pub fn claude_status(cfg: &ClaudeConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "claude".into(),
            name: "Claude".into(),
            available: false,
            coming_soon: false,
            error: Some("Claude is disabled".into()),
            models: vec![],
            default_model: None,
        };
    }
    if !cfg.available() {
        let models = claude_models();
        let default_model = claude_default_model(&models);
        return ChatProviderInfo {
            id: "claude".into(),
            name: "Claude".into(),
            available: false,
            coming_soon: false,
            error: Some("Set ANTHROPIC_API_KEY to enable Claude chat.".into()),
            models,
            default_model,
        };
    }
    let models = claude_models();
    let default_model = claude_default_model(&models);
    ChatProviderInfo {
        id: "claude".into(),
        name: "Claude".into(),
        available: true,
        coming_soon: false,
        error: None,
        models,
        default_model,
    }
}

fn http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::internal(format!("http client: {e}")))
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

async fn emit(progress: &Option<tokio::sync::mpsc::Sender<ChatProgress>>, ev: ChatProgress) {
    if let Some(tx) = progress {
        let _ = tx.send(ev).await;
    }
}

// ── OpenAI (chat completions) ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CompletionsResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<ApiErr>,
}

#[derive(Debug, Deserialize)]
struct ApiErr {
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

async fn openai_completions(
    cfg: &OpenaiConfig,
    model: &str,
    messages: &[Value],
    with_tools: bool,
) -> AppResult<CompletionsResponse> {
    let key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| AppError::bad_request("OPENAI_API_KEY is not set"))?;
    crate::retry::retry_provider("openai", || {
        openai_completions_once(cfg, key, model, messages, with_tools)
    })
    .await
}

async fn openai_completions_once(
    cfg: &OpenaiConfig,
    key: &str,
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
    let res = client
        .post(&url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::bad_request(format!("cannot reach OpenAI: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| AppError::internal(format!("openai body: {e}")))?;
    if !status.is_success() {
        return Err(AppError::bad_request(format!(
            "OpenAI error ({status}): {text}"
        )));
    }
    let parsed: CompletionsResponse = serde_json::from_str(&text)
        .map_err(|e| AppError::internal(format!("openai parse: {e}")))?;
    if let Some(err) = &parsed.error {
        if !err.message.is_empty() {
            return Err(AppError::bad_request(format!("OpenAI: {}", err.message)));
        }
    }
    Ok(parsed)
}

pub async fn gemini_chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &GeminiConfig,
    model: &str,
    history: &[(String, String)],
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
    runner_id: Option<Uuid>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    if !cfg.available() {
        return Err(AppError::bad_request(
            "Gemini is not configured (set GEMINI_API_KEY)".to_string(),
        ));
    }
    let openai = cfg.as_openai();
    let model = if model.trim().is_empty() {
        gemini_default_model(&gemini_models()).unwrap_or_else(|| "gemini-2.5-flash".into())
    } else {
        model.to_string()
    };
    openai_chat_with_tools(
        state,
        workspace_id,
        user_id,
        &openai,
        &model,
        history,
        progress,
        runner_id,
    )
    .await
}

pub async fn openai_chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &OpenaiConfig,
    model: &str,
    history: &[(String, String)],
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
    runner_id: Option<Uuid>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    if !cfg.available() {
        return Err(AppError::bad_request(
            "OpenAI is not configured (set OPENAI_API_KEY)".to_string(),
        ));
    }
    let fallback = openai_default_model(&openai_models()).unwrap_or_default();
    let model = if model.trim().is_empty() {
        if fallback.is_empty() {
            return Err(AppError::bad_request(
                "no OpenAI model listed — set OPENAI_MODELS or pick one from the runner"
                    .to_string(),
            ));
        }
        fallback.as_str()
    } else {
        model.trim()
    };
    emit(
        &progress,
        ChatProgress::Status {
            text: format!("Thinking with {model}…"),
        },
    )
    .await;

    let mut messages: Vec<Value> = Vec::with_capacity(history.len() + 1);
    let system = system_prompt_for_workspace(state, workspace_id).await;
    messages.push(json!({ "role": "system", "content": system }));
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
    let max_rounds = MAX_TOOL_ROUNDS;
    let mut round = 0usize;
    while round < max_rounds {
        let parsed = openai_completions(cfg, model, &messages, true).await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model.clone();
        }
        let choice = parsed
            .choices
            .first()
            .ok_or_else(|| AppError::internal("OpenAI returned no choices".to_string()))?;
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
                content = format!("Done.\n{summary}");
            }
            emit(
                &progress,
                ChatProgress::Delta {
                    text: content.clone(),
                },
            )
            .await;
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
            emit(
                &progress,
                ChatProgress::Status {
                    text: format!("Using {name}…"),
                },
            )
            .await;
            let llm = ChatLlmContext {
                provider: "openai".into(),
                model: model.to_string(),
                runner_id,
                effort: None,
            };
            let prior: Vec<Value> = actions.iter().filter_map(|a| a.detail.clone()).collect();
            let (result_json, action) =
                execute_tool_with_retry(state, workspace_id, user_id, name, &args, &llm, &prior)
                    .await;
            emit(&progress, ChatProgress::from_action(&action)).await;
            actions.push(action);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into()),
            }));
        }
        round += 1;
    }
    let parsed = openai_completions(cfg, model, &messages, false).await?;
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
    emit(
        &progress,
        ChatProgress::Delta {
            text: content.clone(),
        },
    )
    .await;
    Ok((used_model, content, actions))
}

// ── Claude (Anthropic messages) ─────────────────────────────────────────────

fn anthropic_tools() -> Value {
    let defs = tool_definitions();
    let arr = defs.as_array().cloned().unwrap_or_default();
    let tools: Vec<Value> = arr
        .into_iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f.get("name")?,
                "description": f.get("description").cloned().unwrap_or(json!("")),
                "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type":"object"})),
            }))
        })
        .collect();
    Value::Array(tools)
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    content: Vec<AnthropicBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    error: Option<ApiErr>,
}

#[derive(Debug, Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Value,
}

async fn claude_messages(
    cfg: &ClaudeConfig,
    model: &str,
    system: &str,
    messages: &[Value],
    with_tools: bool,
) -> AppResult<AnthropicResponse> {
    let key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| AppError::bad_request("ANTHROPIC_API_KEY is not set"))?;
    crate::retry::retry_provider("claude", || {
        claude_messages_once(cfg, key, model, system, messages, with_tools)
    })
    .await
}

async fn claude_messages_once(
    cfg: &ClaudeConfig,
    key: &str,
    model: &str,
    system: &str,
    messages: &[Value],
    with_tools: bool,
) -> AppResult<AnthropicResponse> {
    let client = http_client()?;
    let url = format!("{}/v1/messages", cfg.api_base);
    let mut body = json!({
        "model": model,
        "max_tokens": 8192,
        "system": system,
        "messages": messages,
    });
    if with_tools {
        body["tools"] = anthropic_tools();
    }
    let res = client
        .post(&url)
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::bad_request(format!("cannot reach Anthropic: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| AppError::internal(format!("claude body: {e}")))?;
    if !status.is_success() {
        return Err(AppError::bad_request(format!(
            "Claude error ({status}): {text}"
        )));
    }
    let parsed: AnthropicResponse = serde_json::from_str(&text)
        .map_err(|e| AppError::internal(format!("claude parse: {e}")))?;
    if let Some(err) = &parsed.error {
        if !err.message.is_empty() {
            return Err(AppError::bad_request(format!("Claude: {}", err.message)));
        }
    }
    Ok(parsed)
}

pub async fn claude_chat_with_tools(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    cfg: &ClaudeConfig,
    model: &str,
    history: &[(String, String)],
    progress: Option<tokio::sync::mpsc::Sender<ChatProgress>>,
    runner_id: Option<Uuid>,
) -> AppResult<(String, String, Vec<ChatAction>)> {
    if !cfg.available() {
        return Err(AppError::bad_request(
            "Claude is not configured (set ANTHROPIC_API_KEY)".to_string(),
        ));
    }
    let fallback = claude_default_model(&claude_models()).unwrap_or_default();
    let model = if model.trim().is_empty() {
        if fallback.is_empty() {
            return Err(AppError::bad_request(
                "no Claude model listed — set CLAUDE_MODELS or pick one from the runner"
                    .to_string(),
            ));
        }
        fallback.as_str()
    } else {
        model.trim()
    };
    emit(
        &progress,
        ChatProgress::Status {
            text: format!("Thinking with {model}…"),
        },
    )
    .await;

    let system = system_prompt_for_workspace(state, workspace_id).await;
    let mut messages: Vec<Value> = Vec::new();
    for (role, content) in history {
        let role = if role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        messages.push(json!({ "role": role, "content": content }));
    }

    let mut actions: Vec<ChatAction> = Vec::new();
    let mut used_model = model.to_string();
    let max_rounds = MAX_TOOL_ROUNDS;
    let mut round = 0usize;
    while round < max_rounds {
        let parsed = claude_messages(cfg, model, &system, &messages, true).await?;
        if !parsed.model.is_empty() {
            used_model = parsed.model.clone();
        }
        let tool_uses: Vec<&AnthropicBlock> = parsed
            .content
            .iter()
            .filter(|b| b.kind == "tool_use")
            .collect();
        let text = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if tool_uses.is_empty() {
            let mut content = text.trim().to_string();
            if let Some(rewritten) = rewrite_if_asked_to_continue(&content, &actions) {
                content = rewritten;
            }
            if content.is_empty() && !actions.is_empty() {
                let summary = actions
                    .iter()
                    .map(|a| format!("• {}: {}", a.tool, a.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                content = format!("Done.\n{summary}");
            }
            emit(
                &progress,
                ChatProgress::Delta {
                    text: content.clone(),
                },
            )
            .await;
            return Ok((used_model, content, actions));
        }

        messages.push(json!({
            "role": "assistant",
            "content": parsed.content.iter().map(|b| {
                if b.kind == "tool_use" {
                    json!({
                        "type": "tool_use",
                        "id": b.id,
                        "name": b.name,
                        "input": b.input,
                    })
                } else {
                    json!({ "type": "text", "text": b.text.clone().unwrap_or_default() })
                }
            }).collect::<Vec<_>>()
        }));

        let mut tool_results: Vec<Value> = Vec::new();
        for call in tool_uses {
            let name = call.name.as_deref().unwrap_or("").trim();
            let args = tool_args_value(&call.input);
            let id = call
                .id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            emit(
                &progress,
                ChatProgress::Status {
                    text: format!("Using {name}…"),
                },
            )
            .await;
            let llm = ChatLlmContext {
                provider: "claude".into(),
                model: model.to_string(),
                runner_id,
                effort: None,
            };
            let prior: Vec<Value> = actions.iter().filter_map(|a| a.detail.clone()).collect();
            let (result_json, action) =
                execute_tool_with_retry(state, workspace_id, user_id, name, &args, &llm, &prior)
                    .await;
            emit(&progress, ChatProgress::from_action(&action)).await;
            actions.push(action);
            tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": serde_json::to_string(&result_json).unwrap_or_else(|_| "{}".into()),
            }));
        }
        messages.push(json!({ "role": "user", "content": tool_results }));
        let _ = parsed.stop_reason;
        round += 1;
    }

    let parsed = claude_messages(cfg, model, &system, &messages, false).await?;
    if !parsed.model.is_empty() {
        used_model = parsed.model;
    }
    let mut content = parsed
        .content
        .iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
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
    emit(
        &progress,
        ChatProgress::Delta {
            text: content.clone(),
        },
    )
    .await;
    Ok((used_model, content, actions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_tools_preserves_every_tool_definitions_name() {
        let openai_tools = tool_definitions();
        let openai_names: Vec<&str> = openai_tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        let claude_tools = anthropic_tools();
        let claude_names: Vec<&str> = claude_tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(claude_names, openai_names);
    }

    #[tokio::test]
    async fn execute_tool_unknown_first_class_github_write_tools() {
        let state = dummy_chat_state();
        let llm = ChatLlmContext {
            provider: "openai".into(),
            model: "test".into(),
            runner_id: None,
            effort: None,
        };
        let args = json!({});
        for name in ["github_commit_files", "github_create_pr"] {
            let (content, action) = crate::chat_tools::execute_tool(
                &state,
                Uuid::nil(),
                Uuid::nil(),
                name,
                &args,
                &llm,
            )
            .await;
            assert!(!action.ok, "{name} must not dispatch");
            assert_eq!(action.tool, name);
            let expected = format!("unknown tool `{name}`");
            assert_eq!(action.summary, expected);
            assert_eq!(content, json!({ "error": expected }));
        }
        state.db.close().await;
    }

    #[test]
    fn openai_status_without_key() {
        let cfg = OpenaiConfig {
            enabled: true,
            api_key: None,
            api_base: OPENAI_DEFAULT_BASE.into(),
        };
        let p = openai_status(&cfg);
        assert!(!p.available);
        assert!(!p.coming_soon);
        let err = p.error.unwrap().to_lowercase();
        assert!(
            err.contains("openai_api_key") || err.contains("codex") || err.contains("cli"),
            "{err}"
        );
    }

    #[test]
    fn openai_status_disabled_is_not_coming_soon() {
        let cfg = OpenaiConfig {
            enabled: false,
            api_key: Some("sk-test".into()),
            api_base: OPENAI_DEFAULT_BASE.into(),
        };
        let p = openai_status(&cfg);
        assert!(!p.available);
        assert!(!p.coming_soon);
        assert!(p.models.is_empty());
        assert!(p.default_model.is_none());
    }

    #[test]
    fn gemini_status_without_key_is_not_coming_soon() {
        let cfg = GeminiConfig {
            enabled: true,
            api_key: None,
            api_base: GEMINI_DEFAULT_BASE.into(),
        };
        let p = gemini_status(&cfg);
        assert!(!p.available);
        assert!(!p.coming_soon);
        assert_eq!(p.id, "gemini");
    }

    #[test]
    fn parse_provider_accepts_gemini() {
        assert_eq!(parse_provider("gemini").unwrap(), "gemini");
        assert_eq!(parse_provider("Google").unwrap(), "gemini");
    }

    #[test]
    fn openai_config_available_requires_enabled_and_nonempty_key() {
        assert!(!OpenaiConfig {
            enabled: false,
            api_key: Some("sk-test".into()),
            api_base: OPENAI_DEFAULT_BASE.into(),
        }
        .available());
        assert!(!OpenaiConfig {
            enabled: true,
            api_key: None,
            api_base: OPENAI_DEFAULT_BASE.into(),
        }
        .available());
        assert!(!OpenaiConfig {
            enabled: true,
            api_key: Some(String::new()),
            api_base: OPENAI_DEFAULT_BASE.into(),
        }
        .available());
        assert!(OpenaiConfig {
            enabled: true,
            api_key: Some("sk-test".into()),
            api_base: OPENAI_DEFAULT_BASE.into(),
        }
        .available());
    }

    #[test]
    fn openai_status_with_key_is_available_not_coming_soon() {
        let cfg = OpenaiConfig {
            enabled: true,
            api_key: Some("sk-test".into()),
            api_base: OPENAI_DEFAULT_BASE.into(),
        };
        let p = openai_status(&cfg);
        assert!(p.available);
        assert!(!p.coming_soon);
        assert!(p.error.is_none());
    }

    /// Empty, whitespace, typo, and ticket-name ids are BadRequest 400.
    #[test]
    fn unknown_empty_typo_provider_fails_closed() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        const RAWS: &[&str] = &[
            "",
            " ",
            "\t",
            "grokk",
            "open-ai",
            "openaii",
            "claud",
            "not-a-real-provider",
            "TASK_PROVIDER",
            "github",
            "jira",
        ];

        for raw in RAWS {
            let err = match parse_provider(raw) {
                Ok(id) => panic!("{raw:?} must be 400, must not resolve to live id {id}"),
                Err(err) => err,
            };
            assert!(
                matches!(err, AppError::BadRequest(_)),
                "{raw:?} must be BadRequest, not {err:?}"
            );
            let status = err.into_response().status();
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{raw:?} must be HTTP 400, got {status}"
            );
        }
    }

    /// Tool-only OpenAI must stop at MAX_TOOL_ROUNDS (plus one no-tools call)
    /// and still return non-empty assistant text.
    #[tokio::test]
    async fn openai_tool_only_stops_at_max_tool_rounds() {
        let sink = ToolOnlyLlm::bind();
        let state = dummy_chat_state();
        state.db.close().await;

        let cfg = OpenaiConfig {
            enabled: true,
            api_key: Some("sk-tool-rounds-test".into()),
            api_base: sink.base.clone(),
        };
        let history = vec![("user".into(), "keep calling tools".into())];

        let (_model, content, actions) = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            openai_chat_with_tools(
                &state,
                Uuid::new_v4(),
                Uuid::new_v4(),
                &cfg,
                "gpt-test",
                &history,
                None,
                None,
            ),
        )
        .await
        .expect("OpenAI tool-only model must stop at MAX_TOOL_ROUNDS, not recurse unbounded")
        .expect("cap hit must return Ok so the assistant turn can persist");

        assert_eq!(actions.len(), MAX_TOOL_ROUNDS);
        assert!(!content.trim().is_empty());
        assert_eq!(sink.hits(), MAX_TOOL_ROUNDS + 1);
        for a in &actions {
            assert_eq!(a.tool, "always_more_tools");
        }
    }

    /// Tool-only Claude must stop at MAX_TOOL_ROUNDS (plus one no-tools call)
    /// and still return non-empty assistant text.
    #[tokio::test]
    async fn claude_tool_only_stops_at_max_tool_rounds() {
        let sink = ToolOnlyLlm::bind();
        let state = dummy_chat_state();
        state.db.close().await;

        let cfg = ClaudeConfig {
            enabled: true,
            api_key: Some("sk-tool-rounds-test".into()),
            api_base: sink.base.clone(),
        };
        let history = vec![("user".into(), "keep calling tools".into())];

        let (_model, content, actions) = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            claude_chat_with_tools(
                &state,
                Uuid::new_v4(),
                Uuid::new_v4(),
                &cfg,
                "claude-test",
                &history,
                None,
                None,
            ),
        )
        .await
        .expect("Claude tool-only model must stop at MAX_TOOL_ROUNDS, not recurse unbounded")
        .expect("cap hit must return Ok so the assistant turn can persist");

        assert_eq!(actions.len(), MAX_TOOL_ROUNDS);
        assert!(!content.trim().is_empty());
        assert_eq!(sink.hits(), MAX_TOOL_ROUNDS + 1);
        for a in &actions {
            assert_eq!(a.tool, "always_more_tools");
        }
    }

    fn dummy_chat_state() -> AppState {
        use std::sync::Arc;
        use std::time::Duration;

        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool (tool-only loop must not need Postgres)");
        let store = fleet_orchestrator::store::Store::new(pool.clone());
        let data_dir = std::env::temp_dir().join(format!("fleet-tool-rounds-{}", Uuid::new_v4()));
        let tick = Arc::new(fleet_orchestrator::orch::TickRunner::new(
            store.clone(),
            data_dir,
            "http://127.0.0.1:0",
        ));
        let orch = fleet_orchestrator::api::AppState {
            store,
            tick,
            admin_token: "test".into(),
            auth_mode: fleet_orchestrator::config::AuthMode::Token,
        };
        let jwt = "dev-change-me-to-a-long-random-secret-at-least-32-chars";
        AppState {
            db: pool,
            auth: fleet_auth::AuthConfig {
                jwt_secret: jwt.into(),
                access_ttl: Duration::from_secs(60),
                refresh_ttl: Duration::from_secs(3600),
                cookie_secure: false,
                cookie_samesite: cookie::SameSite::Lax,
                signup_token: None,
                instance_admin_emails: Vec::new(),
            },
            integrations: fleet_integrations::IntegrationsConfig {
                api_public_base: "http://127.0.0.1:0".into(),
                ui_public_base: "http://127.0.0.1:0".into(),
                state_secret: jwt.into(),
                credentials_key: fleet_integrations::CredentialsKey::from_secret(jwt)
                    .expect("credentials key"),
                github: None,
                gitlab: None,
                gitea: None,
                gitea_base_url: "http://localhost:3033".into(),
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
            },
            orch,
            ollama: crate::chat::OllamaConfig {
                base_url: "http://127.0.0.1:1".into(),
                default_model: None,
                timeout: Duration::from_secs(1),
                enabled: false,
            },
            grok: crate::grok::GrokConfig {
                enabled: false,
                auth_path: std::env::temp_dir().join("fleet-tool-rounds-no-grok-auth.json"),
                api_key: None,
                api_base: "http://127.0.0.1:1".into(),
            },
            openai: OpenaiConfig {
                enabled: true,
                api_key: Some("sk-tool-rounds-test".into()),
                api_base: "http://127.0.0.1:1".into(),
            },
            claude: ClaudeConfig {
                enabled: true,
                api_key: Some("sk-tool-rounds-test".into()),
                api_base: "http://127.0.0.1:1".into(),
            },
            gemini: GeminiConfig {
                enabled: true,
                api_key: Some("sk-tool-rounds-test".into()),
                api_base: "http://127.0.0.1:1".into(),
            },
            worker_dist_dir: None,
        }
    }

    /// Local LLM that only ever returns another tool call.
    struct ToolOnlyLlm {
        base: String,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Drop for ToolOnlyLlm {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl ToolOnlyLlm {
        fn bind() -> Self {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            use std::sync::Arc;
            use std::thread;
            use std::time::Duration;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tool-only llm");
            listener
                .set_nonblocking(true)
                .expect("nonblocking tool-only llm");
            let addr = listener.local_addr().expect("tool-only llm addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
            thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            hits_t.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 4096];
                            loop {
                                match stream.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&tmp[..n]);
                                        if let Some(end) = headers_end(&buf) {
                                            let len = content_length(&buf[..end]).unwrap_or(0);
                                            if buf.len() >= end + len {
                                                break;
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let req = String::from_utf8_lossy(&buf);
                            let body = if req.contains("/v1/messages")
                                || req.contains("anthropic-version")
                                || req.contains("x-api-key")
                            {
                                claude_tool_only_body()
                            } else {
                                openai_tool_only_body()
                            };
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base: format!("http://{addr}"),
                hits,
                stop,
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        let s = std::str::from_utf8(headers).ok()?;
        for line in s.split("\r\n") {
            let (k, v) = line.split_once(':')?;
            if k.eq_ignore_ascii_case("content-length") {
                return v.trim().parse().ok();
            }
        }
        None
    }

    fn openai_tool_only_body() -> String {
        json!({
            "model": "gpt-test",
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_keep_going",
                        "type": "function",
                        "function": {
                            "name": "always_more_tools",
                            "arguments": "{}"
                        }
                    }]
                }
            }]
        })
        .to_string()
    }

    fn claude_tool_only_body() -> String {
        json!({
            "model": "claude-test",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "tu_keep_going",
                "name": "always_more_tools",
                "input": {}
            }]
        })
        .to_string()
    }
}
