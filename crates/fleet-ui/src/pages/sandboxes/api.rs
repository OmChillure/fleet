//! Cookie-auth client for workspace sandboxes (worker sessions).

use serde::Deserialize;

use super::shape::{list_sandboxes_path, SandboxesResponse};
use crate::pages::auth::api::{auth_base_url, with_credentials};

pub use super::shape::SandboxRow;

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

/// List worker sessions as sandboxes for the workspace.
pub async fn list_sandboxes(workspace_id: &str) -> Result<SandboxesResponse, SandboxError> {
    let res = with_credentials(client().get(api_url(&list_sandboxes_path(workspace_id))))
        .send()
        .await
        .map_err(|e| SandboxError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}
