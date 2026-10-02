//! Browser client for workspace chat (Grok + Ollama + saved conversations).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
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
    #[serde(default)]
    pub detail: Option<Value>,
}

/// Wire format from POST /chat (extra fields kept for API stability).
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone)]
pub enum ChatStreamEvent {
    Status { text: String },
    Delta { text: String },
    Action(ChatActionDto),
    Done(ChatCompleteResponse),
    Error { error: String },
}

fn parse_stream_line(line: &str) -> Option<ChatStreamEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let kind = v.get("type")?.as_str()?;
    match kind {
        "status" => Some(ChatStreamEvent::Status {
            text: v.get("text")?.as_str()?.to_string(),
        }),
        "delta" => Some(ChatStreamEvent::Delta {
            text: v.get("text")?.as_str()?.to_string(),
        }),
        "action" => {
            let action = ChatActionDto {
                tool: v.get("tool")?.as_str()?.to_string(),
                ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
                summary: v
                    .get("summary")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                run_id: v
                    .get("run_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                sandbox_ids: v.get("sandbox_ids").and_then(|x| {
                    x.as_array().map(|a| {
                        a.iter()
                            .filter_map(|i| i.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                }),
                detail: v.get("detail").cloned(),
            };
            Some(ChatStreamEvent::Action(action))
        }
        "done" => Some(ChatStreamEvent::Done(ChatCompleteResponse {
            message: {
                let msg = v.get("message");
                let content = msg
                    .and_then(|x| x.as_str())
                    .or_else(|| msg.and_then(|x| x.get("content")).and_then(|x| x.as_str()))
                    .unwrap_or("")
                    .to_string();
                let role = msg
                    .and_then(|x| x.get("role"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("assistant")
                    .to_string();
                ChatMessageDtoOut { role, content }
            },
            model: v
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            provider: v
                .get("provider")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            actions: v
                .get("actions")
                .and_then(|a| serde_json::from_value(a.clone()).ok())
                .unwrap_or_default(),
            conversation_id: v
                .get("conversation_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        })),
        "error" => Some(ChatStreamEvent::Error {
            error: v
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("stream error")
                .to_string(),
        }),
        _ => None,
    }
}

/// Stream a chat turn (NDJSON). Falls back to a single complete on non-wasm.
pub async fn stream_chat(
    workspace_id: &str,
    messages: Vec<ChatMessageDto>,
    provider: String,
    model: Option<String>,
    conversation_id: Option<String>,
    runner_id: Option<String>,
    effort: Option<String>,
    mut on_event: impl FnMut(ChatStreamEvent),
) -> Result<ChatCompleteResponse, ChatError> {
    #[cfg(target_arch = "wasm32")]
    {
        return stream_chat_wasm(
            workspace_id,
            messages,
            provider,
            model,
            conversation_id,
            runner_id,
            effort,
            &mut on_event,
        )
        .await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let res = complete_chat(
            workspace_id,
            messages,
            provider,
            model,
            conversation_id,
            runner_id,
            effort,
        )
        .await?;
        on_event(ChatStreamEvent::Delta {
            text: res.message.content.clone(),
        });
        on_event(ChatStreamEvent::Done(res.clone()));
        Ok(res)
    }
}

#[cfg(target_arch = "wasm32")]
async fn stream_chat_wasm(
    workspace_id: &str,
    messages: Vec<ChatMessageDto>,
    provider: String,
    model: Option<String>,
    conversation_id: Option<String>,
    runner_id: Option<String>,
    effort: Option<String>,
    on_event: &mut impl FnMut(ChatStreamEvent),
) -> Result<ChatCompleteResponse, ChatError> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Request, RequestCredentials, RequestInit, RequestMode};

    let body = ChatCompleteBody {
        messages,
        provider,
        model,
        conversation_id,
        runner_id,
        effort,
    };
    let payload =
        serde_json::to_string(&body).map_err(|e| ChatError::Network(format!("encode: {e}")))?;
    let url = api_url(&format!("/api/workspaces/{workspace_id}/chat/stream"));

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::Cors);
    opts.set_credentials(RequestCredentials::Include);
    opts.set_body(&wasm_bindgen::JsValue::from_str(&payload));

    let request = Request::new_with_str_and_init(&url, &opts)
        .map_err(|_| ChatError::Network("could not build stream request".into()))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|_| ChatError::Network("could not set headers".into()))?;

    let window = web_sys::window().ok_or_else(|| ChatError::Network("no window".into()))?;
    let resp_js = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| ChatError::Network("stream fetch failed".into()))?;
    let resp: web_sys::Response = resp_js
        .dyn_into()
        .map_err(|_| ChatError::Network("invalid stream response".into()))?;
    if !resp.ok() {
        let text = JsFuture::from(
            resp.text()
                .map_err(|_| ChatError::Network("read error body".into()))?,
        )
        .await
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
        return Err(ChatError::Message(if text.is_empty() {
            format!("stream failed ({})", resp.status())
        } else {
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or(text)
        }));
    }
    let body = resp
        .body()
        .ok_or_else(|| ChatError::Network("empty stream body".into()))?;
    let reader = body
        .get_reader()
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| ChatError::Network("no stream reader".into()))?;

    let mut buf = String::new();
    let mut last_done: Option<ChatCompleteResponse> = None;
    loop {
        let read = JsFuture::from(reader.read())
            .await
            .map_err(|_| ChatError::Network("stream read failed".into()))?;
        let done = js_sys::Reflect::get(&read, &"done".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Ok(value) = js_sys::Reflect::get(&read, &"value".into()) {
            if !value.is_undefined() && !value.is_null() {
                if let Some(arr) = value.dyn_ref::<js_sys::Uint8Array>() {
                    let mut bytes = vec![0u8; arr.length() as usize];
                    arr.copy_to(&mut bytes);
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
        while let Some(idx) = buf.find('\n') {
            let line = buf[..idx].trim().to_string();
            buf = buf[idx + 1..].to_string();
            if line.is_empty() {
                continue;
            }
            if let Some(ev) = parse_stream_line(&line) {
                if let ChatStreamEvent::Done(ref d) = ev {
                    last_done = Some(d.clone());
                }
                if let ChatStreamEvent::Error { error } = &ev {
                    return Err(ChatError::Message(error.clone()));
                }
                on_event(ev);
            }
        }
        if done {
            break;
        }
    }
    last_done.ok_or_else(|| ChatError::Network("stream ended without a done event".into()))
}

/// Send conversation turn; persists server-side when successful.
pub async fn complete_chat(
    workspace_id: &str,
    messages: Vec<ChatMessageDto>,
    provider: String,
    model: Option<String>,
    conversation_id: Option<String>,
    runner_id: Option<String>,
    effort: Option<String>,
) -> Result<ChatCompleteResponse, ChatError> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/chat"),
        &ChatCompleteBody {
            messages,
            provider,
            model,
            conversation_id,
            runner_id,
            effort,
        },
    )
    .await?;
    parse_json(res).await
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FleetLiveSnapshot {
    #[serde(default)]
    pub live_agents: u64,
    #[serde(default)]
    pub runs: Vec<Value>,
    #[serde(default)]
    pub tasks: Vec<Value>,
    #[serde(default)]
    pub activity: Vec<Value>,
    #[serde(default)]
    pub chat: Vec<Value>,
}

pub async fn fetch_fleet_live(workspace_id: &str) -> Result<FleetLiveSnapshot, ChatError> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/overview")).await?;
    parse_json(res).await
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FleetTasksSnapshot {
    #[serde(default)]
    pub tasks: Vec<Value>,
}

pub async fn fetch_fleet_tasks(workspace_id: &str) -> Result<FleetTasksSnapshot, ChatError> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/tasks")).await?;
    parse_json(res).await
}

#[cfg(test)]
mod stream_parse_tests {
    use super::*;

    #[test]
    fn parse_stream_line_reads_status_text() {
        let status = parse_stream_line(r#"{"type":"status","text":"thinking"}"#).unwrap();
        match status {
            ChatStreamEvent::Status { text } => assert_eq!(text, "thinking"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stream_line_reads_error_message() {
        let err = parse_stream_line(r#"{"type":"error","error":"nope"}"#).unwrap();
        match err {
            ChatStreamEvent::Error { error } => assert_eq!(error, "nope"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stream_line_reads_string_message_done() {
        let done = parse_stream_line(
            r#"{"type":"done","message":"plain","provider":"grok","conversation_id":"c2"}"#,
        )
        .expect("string message");
        match done {
            ChatStreamEvent::Done(res) => {
                assert_eq!(res.message.content, "plain");
                assert_eq!(res.provider, "grok");
                assert_eq!(res.conversation_id.as_deref(), Some("c2"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stream_line_reads_delta_text() {
        let delta = parse_stream_line(r#"{"type":"delta","text":"tok"}"#).unwrap();
        match delta {
            ChatStreamEvent::Delta { text } => assert_eq!(text, "tok"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stream_line_action_includes_summary_and_run_id() {
        let action = parse_stream_line(
            r#"{"type":"action","tool":"bash","ok":true,"summary":"ran","run_id":"run-1","sandbox_ids":["sb-1"]}"#,
        )
        .unwrap();
        match action {
            ChatStreamEvent::Action(a) => {
                assert_eq!(a.tool, "bash");
                assert!(a.ok);
                assert_eq!(a.summary, "ran");
                assert_eq!(a.run_id.as_deref(), Some("run-1"));
                assert_eq!(
                    a.sandbox_ids.as_deref(),
                    Some(["sb-1".to_string()].as_slice())
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stream_line_unknown_type_and_invalid_json_are_none() {
        assert!(parse_stream_line(r#"{"type":"ping"}"#).is_none());
        assert!(parse_stream_line("not-json").is_none());
    }
}
