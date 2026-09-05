//! Cookie-auth client for workspace sandboxes (worker sessions).

use serde::Deserialize;

use crate::pages::auth::api::{auth_base_url, with_credentials};

#[derive(Debug, Clone)]
pub enum SandboxError {
    Message(String),
    Network(String),
}

impl SandboxError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SandboxRow {
    pub id: String,
    pub run_id: String,
    pub board_task_id: String,
    #[serde(default)]
    pub attempt: i64,
    pub status: String,
    #[serde(default)]
    pub runtime_backend: String,
    #[serde(default)]
    pub runtime_handle: Option<String>,
    #[serde(default)]
    pub worker_profile: String,
    #[serde(default)]
    pub progress_pct: i64,
    #[serde(default)]
    pub work_dir: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub exited_at: Option<String>,
    #[serde(default)]
    pub exit_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxesResponse {
    #[serde(default)]
    pub sandboxes: Vec<SandboxRow>,
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


async fn parse_json<T: for<'de> Deserialize<'de>>(
    res: reqwest::Response,
) -> Result<T, SandboxError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| SandboxError::Network(e.to_string()))?;
    if status.is_success() {
        serde_json::from_str(&text)
            .map_err(|e| SandboxError::Network(format!("invalid JSON: {e} ({text})")))
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
        Err(SandboxError::Message(msg))
    }
}

pub async fn kill_sandbox(workspace_id: &str, session_id: &str) -> Result<(), SandboxError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill"
            )))
            .json(&serde_json::json!({ "reason": "ui_kill" })),
    )
    .send()
    .await
    .map_err(|e| SandboxError::Network(format!("could not reach API: {e}")))?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
}

/// List worker sessions as sandboxes for the workspace.
pub async fn list_sandboxes(workspace_id: &str) -> Result<SandboxesResponse, SandboxError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/workers"
    ))))
    .send()
    .await
    .map_err(|e| SandboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}
