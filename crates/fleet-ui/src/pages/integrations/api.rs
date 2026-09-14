//! Browser client for workspace integrations (OAuth connect / disconnect).

use serde::Deserialize;
use serde_json::json;

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

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

/// GET with credentials; silent refresh once on 401.
async fn authorized_get(path: &str) -> Result<reqwest::Response, IntegrationsError> {
    let first = with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| IntegrationsError::Message(e.message()))?;
    with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))
}

async fn authorized_delete(path: &str) -> Result<reqwest::Response, IntegrationsError> {
    let first = with_credentials(client().delete(api_url(path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| IntegrationsError::Message(e.message()))?;
    with_credentials(client().delete(api_url(path)))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConnectionPublic {
    pub id: String,
    pub workspace_id: String,
    pub provider: String,
    pub status: String,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub defaults: ConnectionDefaults,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ConnectionDefaults {
    #[serde(default)]
    pub default_runner_id: Option<String>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
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
    #[serde(default)]
    pub oauth_callback_url: Option<String>,
    #[serde(default)]
    pub oauth_app_create_url: Option<String>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub connection: Option<ConnectionPublic>,
}

/// Catalog `connected` can lag the nested connection row. Either is enough.
pub fn integration_is_connected(i: &Integration) -> bool {
    if i.connected {
        return true;
    }
    i.connection
        .as_ref()
        .is_some_and(|c| c.connected || c.status.eq_ignore_ascii_case("connected"))
}

pub fn find_integration<'a>(list: &'a [Integration], provider: &str) -> Option<&'a Integration> {
    list.iter()
        .find(|i| i.id.eq_ignore_ascii_case(provider.trim()))
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
pub async fn list_integrations(workspace_id: &str) -> Result<Vec<Integration>, IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations");
    let res = authorized_get(&path).await?;
    let body: IntegrationListResponse = parse_json_or_error(res).await?;
    Ok(body.integrations)
}

/// Absolute URL for full-page OAuth navigation (same-origin in dev via proxy).
pub fn oauth_start_url(workspace_id: &str, provider: &str) -> String {
    api_url(&format!(
        "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"
    ))
}

async fn authorized_patch_json(
    path: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, IntegrationsError> {
    let first = with_credentials(client().patch(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session()
        .await
        .map_err(|e| IntegrationsError::Message(e.message()))?;
    with_credentials(client().patch(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))
}

pub async fn patch_integration_defaults(
    workspace_id: &str,
    provider: &str,
    runner_id: Option<&str>,
    llm_provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations/{provider}");
    let res = authorized_patch_json(
        &path,
        &json!({
            "default_runner_id": runner_id,
            "default_provider": llm_provider,
            "default_model": model,
        }),
    )
    .await?;
    if res.status().is_success() {
        return Ok(());
    }
    parse_json_or_error::<ErrorBody>(res).await?;
    Ok(())
}

/// `DELETE /api/workspaces/{id}/integrations/{provider}`
pub async fn disconnect_integration(
    workspace_id: &str,
    provider: &str,
) -> Result<(), IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations/{provider}");
    let res = authorized_delete(&path).await?;
    if res.status().is_success() {
        Ok(())
    } else {
        let _ = parse_json_or_error::<ErrorBody>(res).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_row_counts_as_connected_when_flag_is_false() {
        let i = Integration {
            id: "slack".into(),
            name: "Slack".into(),
            description: String::new(),
            category: "hitl".into(),
            category_label: "HITL".into(),
            availability: "available".into(),
            icon: "slack".into(),
            docs_url: None,
            auth_method: "oauth".into(),
            oauth_configured: true,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: Some(ConnectionPublic {
                id: "1".into(),
                workspace_id: "w".into(),
                provider: "slack".into(),
                status: "connected".into(),
                account_label: Some("agent-fleet".into()),
                connected: true,
                last_error: None,
                defaults: ConnectionDefaults::default(),
            }),
        };
        assert!(integration_is_connected(&i));
        assert!(find_integration(std::slice::from_ref(&i), "Slack").is_some());
    }

    #[test]
    fn disconnected_without_row_is_not_connected() {
        let i = Integration {
            id: "slack".into(),
            name: "Slack".into(),
            description: String::new(),
            category: "hitl".into(),
            category_label: "HITL".into(),
            availability: "available".into(),
            icon: "slack".into(),
            docs_url: None,
            auth_method: "oauth".into(),
            oauth_configured: true,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        };
        assert!(!integration_is_connected(&i));
    }
}
