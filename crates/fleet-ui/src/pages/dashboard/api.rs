//! Dashboard overview (`GET /api/workspaces/{id}/fleet/overview`).

use serde::Deserialize;
use serde_json::Value;

use crate::pages::auth::api::auth_base_url;

#[derive(Debug, Clone)]
pub enum DashError {
    Message(String),
    Network(String),
}

impl DashError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Overview {
    #[serde(default)]
    pub open_runs: u64,
    #[serde(default)]
    pub total_runs: u64,
    #[serde(default)]
    pub live_agents: u64,
    #[serde(default)]
    pub tasks_in_progress: u64,
    #[serde(default)]
    pub changes_requested: u64,
    #[serde(default)]
    pub focus_run_id: Option<String>,
    #[serde(default)]
    pub runs: Vec<Value>,
    #[serde(default)]
    pub tasks: Vec<Value>,
    #[serde(default)]
    pub waiting: Vec<Value>,
    #[serde(default)]
    pub activity: Vec<Value>,
    #[serde(default)]
    pub chat: Vec<Value>,
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[allow(unused_mut)]
fn with_credentials(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct WorkforceSnapshot {
    #[serde(default)]
    pub open_goals: Vec<Value>,
    #[serde(default)]
    pub goals: Vec<Value>,
}

pub async fn fetch_workforce(workspace_id: &str) -> Result<WorkforceSnapshot, DashError> {
    let url = format!(
        "{}/api/workspaces/{workspace_id}/workforce",
        auth_base_url().trim_end_matches('/')
    );
    let res = with_credentials(client().get(url))
        .send()
        .await
        .map_err(|e| DashError::Network(format!("could not reach API: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| DashError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(DashError::Message(if text.is_empty() {
            format!("request failed ({status})")
        } else {
            text
        }));
    }
    serde_json::from_str(&text).map_err(|e| DashError::Network(format!("invalid JSON: {e}")))
}

pub async fn create_goal(
    workspace_id: &str,
    title: &str,
    goal_md: &str,
) -> Result<Value, DashError> {
    let url = format!(
        "{}/api/workspaces/{workspace_id}/workforce/goals",
        auth_base_url().trim_end_matches('/')
    );
    let res = with_credentials(client().post(url).json(&serde_json::json!({
        "title": title,
        "goal_md": goal_md,
    })))
    .send()
    .await
    .map_err(|e| DashError::Network(format!("could not reach API: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| DashError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(DashError::Message(if text.is_empty() {
            format!("request failed ({status})")
        } else {
            text
        }));
    }
    serde_json::from_str(&text).map_err(|e| DashError::Network(format!("invalid JSON: {e}")))
}

pub async fn fetch_overview(workspace_id: &str) -> Result<Overview, DashError> {
    let url = format!(
        "{}/api/workspaces/{workspace_id}/fleet/overview",
        auth_base_url().trim_end_matches('/')
    );
    let res = with_credentials(client().get(url))
        .send()
        .await
        .map_err(|e| DashError::Network(format!("could not reach API: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| DashError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(DashError::Message(if text.is_empty() {
            format!("request failed ({status})")
        } else {
            text
        }));
    }
    serde_json::from_str(&text).map_err(|e| DashError::Network(format!("invalid JSON: {e}")))
}
