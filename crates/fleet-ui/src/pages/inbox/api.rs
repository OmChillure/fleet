//! Cookie-auth client for the ticket inbox.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pages::auth::api::auth_base_url;

#[derive(Debug, Clone)]
pub enum InboxError {
    Message(String),
    Network(String),
}

impl InboxError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct TicketRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    /// API sends `null` for seeded / URL-less tickets. `String` + default
    /// only covers a missing field, not JSON null — that used to drop the list.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MonitorState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub github_repos: Vec<String>,
    #[serde(default)]
    pub jira_jql: Option<String>,
    #[serde(default)]
    pub slack_channels: Vec<String>,
    #[serde(default)]
    pub notion_databases: Vec<String>,
    #[serde(default)]
    pub auto_launch: bool,
    #[serde(default)]
    pub last_poll_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InboxResponse {
    #[serde(default)]
    pub tickets: Vec<TicketRow>,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub monitor: Option<MonitorState>,
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

async fn parse_json<T: for<'de> Deserialize<'de>>(res: reqwest::Response) -> Result<T, InboxError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| InboxError::Network(e.to_string()))?;
    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| InboxError::Network(format!("invalid JSON: {e}")))
    } else {
        #[derive(Deserialize)]
        struct ErrBody {
            error: String,
        }
        Err(InboxError::Message(
            serde_json::from_str::<ErrBody>(&text)
                .map(|b| b.error)
                .unwrap_or_else(|_| format!("request failed ({status})")),
        ))
    }
}

pub async fn list_inbox(workspace_id: &str) -> Result<InboxResponse, InboxError> {
    let res =
        with_credentials(client().get(api_url(&format!("/api/workspaces/{workspace_id}/inbox"))))
            .send()
            .await
            .map_err(|e| InboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MonitorPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_repos: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_jql: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack_channels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notion_databases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_launch: Option<bool>,
}

pub async fn start_monitor(workspace_id: &str) -> Result<Value, InboxError> {
    update_monitor(
        workspace_id,
        &MonitorPatch {
            enabled: Some(true),
            ..MonitorPatch::default()
        },
    )
    .await
}

pub async fn stop_monitor(workspace_id: &str) -> Result<Value, InboxError> {
    update_monitor(
        workspace_id,
        &MonitorPatch {
            enabled: Some(false),
            ..MonitorPatch::default()
        },
    )
    .await
}

pub async fn update_monitor(workspace_id: &str, patch: &MonitorPatch) -> Result<Value, InboxError> {
    let res =
        with_credentials(client().post(api_url(&format!("/api/workspaces/{workspace_id}/inbox"))))
            .json(patch)
            .send()
            .await
            .map_err(|e| InboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn poll_inbox(workspace_id: &str) -> Result<Value, InboxError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/inbox/poll"
    ))))
    .json(&serde_json::json!({}))
    .send()
    .await
    .map_err(|e| InboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

#[derive(Debug, Clone, Serialize)]
struct FinishBody {
    provider: String,
    key: String,
}

pub async fn finish_ticket(
    workspace_id: &str,
    provider: &str,
    key: &str,
) -> Result<Value, InboxError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/inbox/finish"
    ))))
    .json(&FinishBody {
        provider: provider.to_string(),
        key: key.to_string(),
    })
    .send()
    .await
    .map_err(|e| InboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}
