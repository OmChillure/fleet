//! Browser client for workspace personas.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, refresh_session};

fn api_url(path: &str) -> String {
    format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
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

async fn authorized_get(path: &str) -> Result<reqwest::Response, PersonasError> {
    let first = with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| PersonasError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| PersonasError::Message(e.message()))?;
    with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| PersonasError::Network(format!("could not reach API: {e}")))
}

async fn authorized_json(
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<reqwest::Response, PersonasError> {
    let build = |m: reqwest::Method| {
        let mut b = with_credentials(client().request(m, api_url(path)));
        if let Some(ref v) = body {
            b = b.json(v);
        }
        b
    };
    let first = build(method.clone())
        .send()
        .await
        .map_err(|e| PersonasError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| PersonasError::Message(e.message()))?;
    build(method)
        .send()
        .await
        .map_err(|e| PersonasError::Network(format!("could not reach API: {e}")))
}

#[derive(Debug, Clone)]
pub enum PersonasError {
    Network(String),
    Message(String),
}

impl PersonasError {
    pub fn message(&self) -> String {
        match self {
            Self::Network(s) | Self::Message(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Persona {
    pub id: String,
    pub workspace_id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub role_prompt: String,
    pub worker_profile: String,
    #[serde(default)]
    pub model_preference: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    #[serde(default)]
    pub tools: Value,
    pub is_starter: bool,
    pub is_default: bool,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ListResponse {
    personas: Vec<Persona>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedResponse {
    inserted: u32,
    skipped: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePersonaBody {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_preference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchPersonaBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_preference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

pub fn tools_to_csv(tools: &Value) -> String {
    match tools {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

pub fn csv_to_tools(raw: &str) -> Value {
    Value::Array(
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_string()))
            .collect(),
    )
}

async fn parse_error(res: reqwest::Response) -> PersonasError {
    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(msg) = v.get("error").and_then(|e| e.as_str()) {
            return PersonasError::Message(msg.to_string());
        }
        if let Some(msg) = v.get("message").and_then(|e| e.as_str()) {
            return PersonasError::Message(msg.to_string());
        }
    }
    PersonasError::Message(format!("request failed ({status})"))
}

pub async fn list_personas(workspace_id: &str) -> Result<Vec<Persona>, PersonasError> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/personas")).await?;
    if !res.status().is_success() {
        return Err(parse_error(res).await);
    }
    let body: ListResponse = res
        .json()
        .await
        .map_err(|e| PersonasError::Message(format!("bad response: {e}")))?;
    Ok(body.personas)
}

pub async fn seed_starters(workspace_id: &str) -> Result<(u32, u32), PersonasError> {
    let res = authorized_json(
        reqwest::Method::POST,
        &format!("/api/workspaces/{workspace_id}/personas/seed-starters"),
        None,
    )
    .await?;
    if !res.status().is_success() {
        return Err(parse_error(res).await);
    }
    let body: SeedResponse = res
        .json()
        .await
        .map_err(|e| PersonasError::Message(format!("bad response: {e}")))?;
    Ok((body.inserted, body.skipped))
}

pub async fn create_persona(
    workspace_id: &str,
    body: CreatePersonaBody,
) -> Result<Persona, PersonasError> {
    let res = authorized_json(
        reqwest::Method::POST,
        &format!("/api/workspaces/{workspace_id}/personas"),
        Some(serde_json::to_value(body).unwrap_or(Value::Null)),
    )
    .await?;
    if !res.status().is_success() {
        return Err(parse_error(res).await);
    }
    res.json()
        .await
        .map_err(|e| PersonasError::Message(format!("bad response: {e}")))
}

pub async fn update_persona(
    workspace_id: &str,
    persona_id: &str,
    body: PatchPersonaBody,
) -> Result<Persona, PersonasError> {
    let res = authorized_json(
        reqwest::Method::PATCH,
        &format!("/api/workspaces/{workspace_id}/personas/{persona_id}"),
        Some(serde_json::to_value(body).unwrap_or(Value::Null)),
    )
    .await?;
    if !res.status().is_success() {
        return Err(parse_error(res).await);
    }
    res.json()
        .await
        .map_err(|e| PersonasError::Message(format!("bad response: {e}")))
}

pub async fn archive_persona(workspace_id: &str, persona_id: &str) -> Result<(), PersonasError> {
    let res = authorized_json(
        reqwest::Method::DELETE,
        &format!("/api/workspaces/{workspace_id}/personas/{persona_id}"),
        None,
    )
    .await?;
    if res.status().as_u16() == 204 || res.status().is_success() {
        return Ok(());
    }
    Err(parse_error(res).await)
}
