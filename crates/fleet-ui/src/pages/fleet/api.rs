//! Client for standalone fleet-orchestrator HTTP API (local play).
//!
//! Defaults: orchestrator `http://localhost:8081`, admin token from localStorage
//! or build-time `FLEET_ORCH_TOKEN` / `FLEET_ORCH_URL`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const ORCH_URL_KEY: &str = "fleet_orch_url";
const ORCH_TOKEN_KEY: &str = "fleet_orch_token";
const ORCH_PROJECT_KEY: &str = "fleet_orch_project_id";

pub fn default_orch_url() -> String {
    option_env!("FLEET_ORCH_URL")
        .unwrap_or("http://localhost:8081")
        .to_string()
}

pub fn default_orch_token() -> String {
    option_env!("FLEET_ORCH_TOKEN")
        .unwrap_or("dev-admin-token-change-me")
        .to_string()
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn load_settings() -> (String, String, String) {
    let store = storage();
    let url = store
        .as_ref()
        .and_then(|s| s.get_item(ORCH_URL_KEY).ok().flatten())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_orch_url);
    let token = store
        .as_ref()
        .and_then(|s| s.get_item(ORCH_TOKEN_KEY).ok().flatten())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_orch_token);
    let project = store
        .as_ref()
        .and_then(|s| s.get_item(ORCH_PROJECT_KEY).ok().flatten())
        .unwrap_or_default();
    (url, token, project)
}

pub fn save_settings(url: &str, token: &str, project_id: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(ORCH_URL_KEY, url);
        let _ = s.set_item(ORCH_TOKEN_KEY, token);
        let _ = s.set_item(ORCH_PROJECT_KEY, project_id);
    }
}

#[derive(Debug, Clone)]
pub enum OrchError {
    Message(String),
    Network(String),
}

impl OrchError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

async fn parse(res: reqwest::Response) -> Result<Value, OrchError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| OrchError::Network(e.to_string()))?;
    if status.is_success() {
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| {
            OrchError::Network(format!("invalid JSON: {e} ({text})"))
        })
    } else {
        let msg = serde_json::from_str::<ErrorBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| {
                if text.is_empty() {
                    format!("request failed ({status})")
                } else {
                    text
                }
            });
        Err(OrchError::Message(msg))
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

async fn admin_req(
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, OrchError> {
    let mut req = client()
        .request(method, url(base, path))
        .header("Authorization", format!("Bearer {token}"));
    if let Some(b) = body {
        req = req.json(&b);
    }
    let res = req
        .send()
        .await
        .map_err(|e| OrchError::Network(format!("could not reach orchestrator: {e}")))?;
    parse(res).await
}

pub async fn healthz(base: &str) -> Result<Value, OrchError> {
    let res = client()
        .get(url(base, "/healthz"))
        .send()
        .await
        .map_err(|e| OrchError::Network(format!("could not reach orchestrator: {e}")))?;
    parse(res).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateProjectBody {
    pub name: String,
    pub max_agents: i32,
}

pub async fn create_project(
    base: &str,
    token: &str,
    body: CreateProjectBody,
) -> Result<Value, OrchError> {
    admin_req(
        base,
        token,
        reqwest::Method::POST,
        "/v1/projects",
        Some(serde_json::to_value(body).unwrap()),
    )
    .await
}

pub async fn get_project(base: &str, token: &str, project_id: &str) -> Result<Value, OrchError> {
    admin_req(
        base,
        token,
        reqwest::Method::GET,
        &format!("/v1/projects/{project_id}"),
        None,
    )
    .await
}

pub async fn list_tasks(base: &str, token: &str, project_id: &str) -> Result<Value, OrchError> {
    admin_req(
        base,
        token,
        reqwest::Method::GET,
        &format!("/v1/projects/{project_id}/tasks"),
        None,
    )
    .await
}

pub async fn list_runs(base: &str, token: &str, project_id: &str) -> Result<Value, OrchError> {
    admin_req(
        base,
        token,
        reqwest::Method::GET,
        &format!("/v1/projects/{project_id}/runs"),
        None,
    )
    .await
}

pub async fn admin_tick(base: &str, token: &str, project_id: &str) -> Result<Value, OrchError> {
    admin_req(
        base,
        token,
        reqwest::Method::POST,
        &format!("/v1/projects/{project_id}/admin/tick"),
        Some(json!({})),
    )
    .await
}

pub async fn fixture_two_task_chain(
    base: &str,
    token: &str,
    project_id: &str,
) -> Result<Value, OrchError> {
    let run_id = uuid_v4();
    let plan_id = uuid_v4();
    let plan = json!({
        "plan_id": plan_id,
        "project_id": project_id,
        "run_id": run_id,
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [
            {
                "plan_task_id": "t1",
                "title": "Root task",
                "depends_on": [],
                "worker_profile": "coding",
                "priority": 100,
                "max_retries": 1
            },
            {
                "plan_task_id": "t2",
                "title": "Child task",
                "depends_on": ["t1"],
                "worker_profile": "coding",
                "priority": 50
            }
        ],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    admin_req(
        base,
        token,
        reqwest::Method::POST,
        &format!("/v1/projects/{project_id}/runs/fixture"),
        Some(json!({ "plan": plan })),
    )
    .await
}

pub async fn cancel_latest_run(
    base: &str,
    token: &str,
    project_id: &str,
) -> Result<Value, OrchError> {
    let runs = list_runs(base, token, project_id).await?;
    let run_id = runs["runs"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r["id"].as_str())
        .ok_or_else(|| OrchError::Message("no runs to cancel".into()))?
        .to_string();
    admin_req(
        base,
        token,
        reqwest::Method::POST,
        &format!("/v1/projects/{project_id}/runs/{run_id}/cancel"),
        Some(json!({ "reason": "ui_stop" })),
    )
    .await
}

fn uuid_v4() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Ok(v) = js_sys::eval(
            "(typeof crypto !== 'undefined' && crypto.randomUUID) ? crypto.randomUUID() : null",
        ) {
            if let Some(s) = v.as_string() {
                return s;
            }
        }
        // fallback
        let n = (js_sys::Date::now() as u64) ^ 0x9e37;
        return format!("00000000-0000-4000-a000-{:012x}", n & 0xffffffffffff);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        format!("00000000-0000-4000-a000-{:012x}", n & 0xffffffffffff)
    }
}

use serde_json::json;
