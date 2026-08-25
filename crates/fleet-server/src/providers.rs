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

pub fn openai_status(cfg: &OpenaiConfig) -> ChatProviderInfo {
    if !cfg.enabled {
        return ChatProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            available: false,
            coming_soon: false,
            error: Some("OpenAI is disabled (OPENAI_ENABLED=false)".into()),
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
            error: Some("Set OPENAI_API_KEY to enable OpenAI chat.".into()),
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
        assert!(!p.coming_soon);
        assert!(p.error.unwrap().contains("OPENAI_API_KEY"));
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
}
