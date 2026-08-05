//! Browser client for workspace chat (local Ollama via fleet-server).

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session};

#[derive(Debug, Clone)]
pub enum ChatError {
    Message(String),
    Network(String),
}

impl ChatError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn api_url(path: &str) -> String {
    format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[allow(unused_mut)]
fn with_credentials(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req
}

async fn authorized_get(path: &str) -> Result<reqwest::Response, ChatError> {
    let first = with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| ChatError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| ChatError::Message(e.message()))?;
    with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| ChatError::Network(format!("could not reach API: {e}")))
}

async fn authorized_post_json(
    path: &str,
    body: &impl Serialize,
) -> Result<reqwest::Response, ChatError> {
    let first = with_credentials(client().post(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| ChatError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| ChatError::Message(e.message()))?;
    with_credentials(client().post(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| ChatError::Network(format!("could not reach API: {e}")))
}

async fn parse_json<T: for<'de> Deserialize<'de>>(res: reqwest::Response) -> Result<T, ChatError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| ChatError::Network(e.to_string()))?;
    if status.is_success() {
        serde_json::from_str(&text)
            .map_err(|e| ChatError::Network(format!("invalid JSON: {e} ({text})")))
    } else {
        #[derive(Deserialize)]
        struct ErrBody {
            error: String,
        }
        let msg = serde_json::from_str::<ErrBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| {
                if text.is_empty() {
                    format!("request failed ({status})")
                } else {
                    text
                }
            });
        Err(ChatError::Message(msg))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatModelInfo {
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatProviderInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
    #[serde(default)]
    pub coming_soon: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub models: Vec<ChatModelInfo>,
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatStatus {
    pub available: bool,
    pub enabled: bool,
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: Vec<ChatModelInfo>,
    #[serde(default)]
    pub providers: Vec<ChatProviderInfo>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageDto {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompleteBody {
    pub messages: Vec<ChatMessageDto>,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompleteResponse {
    pub message: ChatMessageDtoOut,
    #[allow(dead_code)]
    pub model: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessageDtoOut {
    #[allow(dead_code)]
    pub role: String,
    pub content: String,
}

/// Probe providers + live Ollama model list (`ollama list` via server).
pub async fn fetch_chat_status(workspace_id: &str) -> Result<ChatStatus, ChatError> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/chat/status")).await?;
    parse_json(res).await
}

/// Send conversation; `model` must be a name from the provider's list.
pub async fn complete_chat(
    workspace_id: &str,
    messages: Vec<ChatMessageDto>,
    provider: String,
    model: Option<String>,
) -> Result<ChatCompleteResponse, ChatError> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/chat"),
        &ChatCompleteBody {
            messages,
            provider,
            model,
        },
    )
    .await?;
    parse_json(res).await
}
