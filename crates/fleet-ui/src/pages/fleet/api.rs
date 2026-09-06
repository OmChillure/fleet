//! Cookie-auth fleet API under `/api/workspaces/{workspace_id}/fleet/*`.

use serde::Deserialize;
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, with_credentials};

#[derive(Debug, Clone)]
pub enum FleetError {
    Message(String),
    Network(String),
}

impl FleetError {
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

/// POST body for Fleet → Start 2-task run (`…/fleet/runs/fixture`).
///
/// Docs §6.5: UI sends `kind=chain` only. `on_task_fail=fail_run`.
/// Starting the fixture does not tick.
pub fn start_fixture_body() -> Value {
    serde_json::json!({
        "kind": "chain",
        "on_task_fail": "fail_run",
    })
}


async fn parse(res: reqwest::Response) -> Result<Value, FleetError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| FleetError::Network(e.to_string()))?;
    if status.is_success() {
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .map_err(|e| FleetError::Network(format!("invalid JSON: {e} ({text})")))
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
        Err(FleetError::Message(msg))
    }
}

pub async fn ensure_fleet(workspace_id: &str) -> Result<Value, FleetError> {
    let res =
        with_credentials(client().get(api_url(&format!("/api/workspaces/{workspace_id}/fleet"))))
            .send()
            .await
            .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn start_fixture(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/runs/fixture"
            )))
            .json(&start_fixture_body()),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn run_tick(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/tick"
            )))
            .json(&serde_json::json!({})),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_tasks(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/tasks"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_runs(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/runs"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_sessions(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/sessions"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn resume_task(
    workspace_id: &str,
    task_id: &str,
    message: &str,
) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/tasks/{task_id}/resume"
            )))
            .json(&serde_json::json!({ "message": message })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn replace_session(workspace_id: &str, session_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/replace"
            )))
            .json(&serde_json::json!({ "reason": "ui_replace" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn kill_session(workspace_id: &str, session_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill"
            )))
            .json(&serde_json::json!({ "reason": "ui_kill" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn cancel_run(workspace_id: &str, run_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/runs/{run_id}/cancel"
            )))
            .json(&serde_json::json!({ "reason": "ui_stop" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}
