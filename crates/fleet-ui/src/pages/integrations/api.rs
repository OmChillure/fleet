//! Browser client for workspace integrations (OAuth connect / disconnect).

#![allow(dead_code)]

use serde::Deserialize;

use crate::pages::auth::api::auth_base_url;

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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConnectionPublic {
    pub id: String,
    pub workspace_id: String,
    pub provider: String,
    pub status: String,
    #[serde(default)]
    pub account_label: Option<String>,
    pub connected: bool,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Integration {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub category_label: String,
    pub availability: String,
    pub icon: String,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub auth_method: String,
    pub oauth_configured: bool,
    #[serde(default)]
    pub oauth_start_path: Option<String>,
    pub connected: bool,
    #[serde(default)]
    pub connection: Option<ConnectionPublic>,
}

#[derive(Debug, Clone, Deserialize)]
struct IntegrationListResponse {
    integrations: Vec<Integration>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Clone)]
pub enum IntegrationsError {
    Message(String),
    Network(String),
}

impl IntegrationsError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

async fn parse_json_or_error<T: for<'de> Deserialize<'de>>(
    res: reqwest::Response,
) -> Result<T, IntegrationsError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| IntegrationsError::Network(e.to_string()))?;

    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| {
            IntegrationsError::Network(format!("invalid success response: {e} ({text})"))
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
        Err(IntegrationsError::Message(msg))
    }
}

/// `GET /api/workspaces/{id}/integrations`
pub async fn list_integrations(
    workspace_id: &str,
) -> Result<Vec<Integration>, IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations");
    let res = with_credentials(client().get(api_url(&path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    let body: IntegrationListResponse = parse_json_or_error(res).await?;
    Ok(body.integrations)
}

/// Absolute URL for full-page OAuth navigation (cookies sent to API origin).
pub fn oauth_start_url(workspace_id: &str, provider: &str) -> String {
    api_url(&format!(
        "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"
    ))
}

/// `DELETE /api/workspaces/{id}/integrations/{provider}`
pub async fn disconnect_integration(
    workspace_id: &str,
    provider: &str,
) -> Result<(), IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations/{provider}");
    let res = with_credentials(client().delete(api_url(&path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    if res.status().is_success() {
        Ok(())
    } else {
        let _ = parse_json_or_error::<ErrorBody>(res).await?;
        Ok(())
    }
}
