//! Cookie-auth client for workspace cases.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, with_credentials};

#[derive(Debug, Clone)]
pub enum CaseError {
    Message(String),
    Network(String),
}

impl CaseError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CaseEventRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CaseRow {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub policy_md: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub context: Value,
    #[serde(default)]
    pub next_wake_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub decision_count: i64,
    #[serde(default)]
    pub events: Vec<CaseEventRow>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CaseListResponse {
    #[serde(default)]
    pub cases: Vec<CaseRow>,
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserveBody {
    pub note: String,
    pub wake: bool,
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

async fn parse_json<T: for<'de> Deserialize<'de>>(res: reqwest::Response) -> Result<T, CaseError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| CaseError::Network(e.to_string()))?;
    if status.is_success() {
        serde_json::from_str(&text)
            .map_err(|e| CaseError::Network(format!("invalid JSON: {e} ({text})")))
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
        Err(CaseError::Message(msg))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateCaseBody {
    pub title: String,
    pub policy_md: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

pub async fn create_case(
    workspace_id: &str,
    title: &str,
    policy_md: &str,
    kind: Option<&str>,
) -> Result<CaseRow, CaseError> {
    let res =
        with_credentials(client().post(api_url(&format!("/api/workspaces/{workspace_id}/cases"))))
            .json(&CreateCaseBody {
                title: title.to_string(),
                policy_md: policy_md.to_string(),
                kind: kind.map(|s| s.to_string()),
                stage: None,
            })
            .send()
            .await
            .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn list_cases(workspace_id: &str) -> Result<CaseListResponse, CaseError> {
    let res =
        with_credentials(client().get(api_url(&format!("/api/workspaces/{workspace_id}/cases"))))
            .send()
            .await
            .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn get_case(workspace_id: &str, case_id: &str) -> Result<CaseRow, CaseError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/cases/{case_id}"
    ))))
    .send()
    .await
    .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn observe_case(
    workspace_id: &str,
    case_id: &str,
    note: &str,
) -> Result<CaseRow, CaseError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/cases/{case_id}/observe"
    ))))
    .json(&ObserveBody {
        note: note.to_string(),
        wake: true,
    })
    .send()
    .await
    .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn wake_case(workspace_id: &str, case_id: &str) -> Result<CaseRow, CaseError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/cases/{case_id}/wake"
    ))))
    .json(&serde_json::json!({}))
    .send()
    .await
    .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn decide_case(
    workspace_id: &str,
    case_id: &str,
    action: &str,
    note: Option<&str>,
    stage: Option<&str>,
) -> Result<Value, CaseError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/cases/{case_id}/decide"
    ))))
    .json(&serde_json::json!({
        "decision": {
            "action": action,
            "note": note,
            "stage": stage,
        }
    }))
    .send()
    .await
    .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}

pub async fn cancel_case(workspace_id: &str, case_id: &str) -> Result<CaseRow, CaseError> {
    let res = with_credentials(client().post(api_url(&format!(
        "/api/workspaces/{workspace_id}/cases/{case_id}/cancel"
    ))))
    .json(&serde_json::json!({}))
    .send()
    .await
    .map_err(|e| CaseError::Network(format!("could not reach API: {e}")))?;
    parse_json(res).await
}
