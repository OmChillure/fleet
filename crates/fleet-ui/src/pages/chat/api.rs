//! Browser client for workspace chat (Grok + Ollama + saved conversations).

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

async fn authorized_delete(path: &str) -> Result<reqwest::Response, ChatError> {
    let first = with_credentials(client().delete(api_url(path)))
        .send()
        .await
        .map_err(|e| ChatError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| ChatError::Message(e.message()))?;
    with_credentials(client().delete(api_url(path)))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatActionDto {
    pub tool: String,
    pub ok: bool,
    pub summary: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub sandbox_ids: Option<Vec<String>>,
}

/// Wire format from POST /chat (extra fields kept for API stability).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ChatCompleteResponse {
    pub message: ChatMessageDtoOut,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub actions: Vec<ChatActionDto>,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ChatMessageDtoOut {
    #[serde(default)]
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConversationDetail {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// Wire format for loaded conversation messages.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct StoredMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub actions: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationsListResponse {
    conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationGetResponse {
    conversation: ConversationDetail,
    messages: Vec<StoredMessage>,
}

/// Probe providers + model lists (Grok CLI auth · Ollama tags).
pub async fn fetch_chat_status(workspace_id: &str) -> Result<ChatStatus, ChatError> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/chat/status")).await?;
    parse_json(res).await
}

/// List saved chats for Sessions page.
pub async fn list_conversations(workspace_id: &str) -> Result<Vec<ConversationSummary>, ChatError> {
    let res = authorized_get(&format!(
        "/api/workspaces/{workspace_id}/chat/conversations"
    ))
    .await?;
    let body: ConversationsListResponse = parse_json(res).await?;
    Ok(body.conversations)
}

#[derive(Debug, Clone, Serialize)]
struct CreateConversationBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateConversationResponse {
    conversation: ConversationDetail,
}

/// Create an empty conversation (used so first message can switch URL immediately).
pub async fn create_conversation(
    workspace_id: &str,
    title: Option<String>,
    provider: Option<String>,
    model: Option<String>,
) -> Result<ConversationDetail, ChatError> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/chat/conversations"),
        &CreateConversationBody {
            title,
            provider,
            model,
        },
    )
    .await?;
    let body: CreateConversationResponse = parse_json(res).await?;
    Ok(body.conversation)
}

/// Load one conversation + messages.
pub async fn get_conversation(
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(ConversationDetail, Vec<StoredMessage>), ChatError> {
    let res = authorized_get(&format!(
        "/api/workspaces/{workspace_id}/chat/conversations/{conversation_id}"
    ))
    .await?;
    let body: ConversationGetResponse = parse_json(res).await?;
    Ok((body.conversation, body.messages))
}

/// Delete a conversation.
pub async fn delete_conversation(
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), ChatError> {
    let res = authorized_delete(&format!(
        "/api/workspaces/{workspace_id}/chat/conversations/{conversation_id}"
    ))
    .await?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
}

/// Send conversation turn; persists server-side when successful.
pub async fn complete_chat(
    workspace_id: &str,
    messages: Vec<ChatMessageDto>,
    provider: String,
    model: Option<String>,
    conversation_id: Option<String>,
) -> Result<ChatCompleteResponse, ChatError> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/chat"),
        &ChatCompleteBody {
            messages,
            provider,
            model,
            conversation_id,
        },
    )
    .await?;
    parse_json(res).await
}
