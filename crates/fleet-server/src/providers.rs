//! OpenAI and Anthropic (Claude) chat providers — same tool loop as Grok.

use std::env;
use std::time::Duration;

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

const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
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
        "claude" | "anthropic" => Ok("claude".into()),
        "grok" | "xai" => Ok("grok".into()),
        "ollama" => Ok("ollama".into()),
        other => Err(AppError::bad_request(format!(
            "unknown provider `{other}` — use grok, ollama, openai, or claude"
        ))),
    }
}

pub fn openai_status(cfg: &OpenaiConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            available: false,
            coming_soon: true,
            error: Some("OpenAI is coming soon".into()),
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
            coming_soon: true,
            error: Some("OpenAI is coming soon".into()),
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
    fn openai_status_without_key() {
        let cfg = OpenaiConfig {
            enabled: true,
            api_key: None,
            api_base: OPENAI_DEFAULT_BASE.into(),
        };
        let p = openai_status(&cfg);
        assert!(!p.available);
        assert!(p.coming_soon);
        assert!(p.error.unwrap().to_lowercase().contains("coming soon"));
    }

    #[test]
    fn openai_status_disabled_is_coming_soon() {
        let cfg = OpenaiConfig {
            enabled: false,
            api_key: None,
            api_base: OPENAI_DEFAULT_BASE.into(),
        };
        let p = openai_status(&cfg);
        assert!(!p.available);
        assert!(p.coming_soon);
        assert!(p.error.unwrap().to_lowercase().contains("coming soon"));
    }

    #[test]
    fn claude_status_without_key() {
        let cfg = ClaudeConfig {
            enabled: true,
            api_key: None,
            api_base: ANTHROPIC_DEFAULT_BASE.into(),
        };
        let p = claude_status(&cfg);
        assert!(!p.available);
        assert!(!p.coming_soon);
    }

    /// Empty / typo / ticket-name composer ids are 400. `parse_provider` is the
    /// single gate: `chat_complete` and `run_chat_turn` must call it (no second
    /// raw match that could map grokk/open-ai/"" onto a live backend).
    #[test]
    fn unknown_empty_typo_provider_fails_closed() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        const LIVE: &[&str] = &["openai", "claude", "grok", "ollama", "anthropic", "xai"];
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
            match parse_provider(raw) {
                Ok(id) => panic!("{raw:?} must be 400, must not resolve to live id {id}"),
                Err(err) => {
                    let status = err.into_response().status();
                    assert_eq!(
                        status,
                        StatusCode::BAD_REQUEST,
                        "{raw:?} must be HTTP 400, got {status}"
                    );
                }
            }
            // Re-parse for the message (into_response consumes the error).
            match parse_provider(raw) {
                Ok(id) => panic!("{raw:?} must stay rejected, got Ok({id})"),
                Err(AppError::BadRequest(msg)) => {
                    let lower = msg.to_ascii_lowercase();
                    assert!(
                        lower.contains("unknown"),
                        "{raw:?} reject must say unknown, got {msg}"
                    );
                    assert!(
                        lower.contains("provider"),
                        "{raw:?} reject must say provider, got {msg}"
                    );
                    let token = raw.trim().to_ascii_lowercase();
                    if !token.is_empty() {
                        assert!(
                            lower.contains(&token),
                            "{raw:?} reject must name the rejected id, got {msg}"
                        );
                    }
                    for live in LIVE {
                        assert_ne!(
                            lower.trim(),
                            *live,
                            "{raw:?} error body must not be the live id {live}"
                        );
                    }
                }
                Err(other) => panic!("{raw:?} must be 400 BadRequest, not {other:?}"),
            }
        }

        assert_eq!(parse_provider("openai").unwrap(), "openai");
        assert_eq!(parse_provider(" OPENAI ").unwrap(), "openai");
        assert_eq!(parse_provider("claude").unwrap(), "claude");
        assert_eq!(parse_provider("anthropic").unwrap(), "claude");
        assert_eq!(parse_provider("grok").unwrap(), "grok");
        assert_eq!(parse_provider("xai").unwrap(), "grok");
        assert_eq!(parse_provider("ollama").unwrap(), "ollama");

        let chat = include_str!("chat.rs");
        let complete = fn_src(chat, "async fn chat_complete(");
        let turn = fn_src(chat, "async fn run_chat_turn(");

        for (label, body) in [("chat_complete", complete), ("run_chat_turn", turn)] {
            let parse_at = pos(body, "parse_provider(", label);
            let cli_at = pos(body, "runner_cli_provider", label);
            let match_at = pos(body, "match provider", label);
            assert!(
                parse_at < cli_at,
                "{label} must parse_provider before runner_cli_provider:\n{body}"
            );
            assert!(
                parse_at < match_at,
                "{label} must parse_provider before the backend match:\n{body}"
            );
            assert!(
                !body.contains(".unwrap_or(\"ollama\").trim()"),
                "{label} must not lowercase/trim the raw then match; empty would become ollama:\n{body}"
            );
            assert!(
                !body.contains("unwrap_or_else"),
                "{label} must not fall through a typo to a live default:\n{body}"
            );
            for typo in [
                "grokk",
                "open-ai",
                "openaii",
                "claud",
                "github",
                "jira",
                "TASK_PROVIDER",
            ] {
                assert!(
                    !body.contains(&format!("\"{typo}\"")),
                    "{label} must not alias {typo:?} onto a live backend:\n{body}"
                );
            }
        }

        assert!(
            complete.contains("body.provider.as_deref().unwrap_or(\"ollama\")"),
            "missing composer provider may default to ollama; empty/typo must still hit parse_provider:\n{complete}"
        );
        assert!(
            !turn.contains("unwrap_or(\"ollama\")"),
            "run_chat_turn must not default a typo/empty id to ollama:\n{turn}"
        );
    }

    /// Docs §6.2/§7: a model that only emits tool calls must stop at
    /// `MAX_TOOL_ROUNDS` and still return text so `chat_complete_*` can
    /// persist the assistant turn. Not a source grep — a tool-only sink.
    #[tokio::test]
    async fn chat_complete_tool_rounds_capped() {
        use crate::chat_tools::MAX_TOOL_ROUNDS;
        use std::time::{Duration, Instant};

        assert!(
            (1..=32).contains(&MAX_TOOL_ROUNDS),
            "MAX_TOOL_ROUNDS must be a small finite cap, got {MAX_TOOL_ROUNDS}"
        );

        let sink = ToolOnlyLlm::bind();
        let state = dummy_chat_state();
        state.db.close().await;

        let openai_cfg = OpenaiConfig {
            enabled: true,
            api_key: Some("sk-tool-rounds-test".into()),
            api_base: sink.base.clone(),
        };
        let claude_cfg = ClaudeConfig {
            enabled: true,
            api_key: Some("sk-tool-rounds-test".into()),
            api_base: sink.base.clone(),
        };
        let history = vec![("user".into(), "keep calling tools".into())];
        let ws = Uuid::new_v4();
        let user = Uuid::new_v4();
        let budget = Duration::from_secs(20);

        let started = Instant::now();
        let openai = tokio::time::timeout(
            budget,
            openai_chat_with_tools(
                &state,
                ws,
                user,
                &openai_cfg,
                "gpt-test",
                &history,
                None,
                None,
            ),
        )
        .await
        .expect("OpenAI tool-only model must stop at MAX_TOOL_ROUNDS, not recurse unbounded")
        .expect("cap hit must return Ok so the assistant turn can persist");
        let openai_hits = sink.hits();
        assert_tool_only_stopped("openai", openai, openai_hits);
        assert!(
            started.elapsed() < budget,
            "OpenAI cap path took {:?}",
            started.elapsed()
        );

        sink.reset_hits();
        let started = Instant::now();
        let claude = tokio::time::timeout(
            budget,
            claude_chat_with_tools(
                &state,
                ws,
                user,
                &claude_cfg,
                "claude-test",
                &history,
                None,
                None,
            ),
        )
        .await
        .expect("Claude tool-only model must stop at MAX_TOOL_ROUNDS, not recurse unbounded")
        .expect("cap hit must return Ok so the assistant turn can persist");
        assert_tool_only_stopped("claude", claude, sink.hits());
        assert!(
            started.elapsed() < budget,
            "Claude cap path took {:?}",
            started.elapsed()
        );

        let chat = include_str!("chat.rs");
        for (label, sig) in [
            ("chat_complete_openai", "async fn chat_complete_openai("),
            ("chat_complete_claude", "async fn chat_complete_claude("),
        ] {
            let body = fn_src(chat, sig);
            let persist_at = pos(body, "persist_user_and_assistant", label);
            let loop_at = if label.contains("openai") {
                pos(body, "openai_chat_with_tools", label)
            } else {
                pos(body, "claude_chat_with_tools", label)
            };
            assert!(
                loop_at < persist_at,
                "{label} must persist the assistant turn after the capped loop:\n{body}"
            );
        }
    }

    fn assert_tool_only_stopped(
        label: &str,
        result: (String, String, Vec<ChatAction>),
        provider_hits: usize,
    ) {
        let (_model, content, actions) = result;
        assert_eq!(
            actions.len(),
            MAX_TOOL_ROUNDS,
            "{label}: tool-only model must run exactly MAX_TOOL_ROUNDS tool rounds, got {}",
            actions.len()
        );
        assert!(
            provider_hits == MAX_TOOL_ROUNDS || provider_hits == MAX_TOOL_ROUNDS + 1,
            "{label}: provider must be called MAX_TOOL_ROUNDS times (optional final no-tools summary), got {provider_hits}"
        );
        assert!(
            !content.trim().is_empty(),
            "{label}: cap path must return assistant text to persist, got empty"
        );
        let lower = content.to_ascii_lowercase();
        assert!(
            lower.contains("tool round limit") || lower.contains("reached"),
            "{label}: empty tool-only model must surface the cap (not drop the turn): {content}"
        );
        for a in &actions {
            assert_eq!(
                a.tool, "always_more_tools",
                "{label}: unexpected tool {a:?}"
            );
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
            },
            integrations: fleet_integrations::IntegrationsConfig {
                api_public_base: "http://127.0.0.1:0".into(),
                ui_public_base: "http://127.0.0.1:0".into(),
                state_secret: jwt.into(),
                credentials_key: fleet_integrations::CredentialsKey::from_secret(jwt)
                    .expect("credentials key"),
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
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

        fn reset_hits(&self) {
            self.hits.store(0, std::sync::atomic::Ordering::SeqCst);
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

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\nasync fn ")
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn pos(hay: &str, needle: &str, label: &str) -> usize {
        hay.find(needle)
            .unwrap_or_else(|| panic!("{label} must contain `{needle}`:\n{hay}"))
    }
}
