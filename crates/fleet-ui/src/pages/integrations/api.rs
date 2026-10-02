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
    pub display_name: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
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
    #[serde(default)]
    pub category_id: String,
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
    #[serde(default)]
    pub connections: Vec<ConnectionPublic>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IntegrationCategory {
    pub id: String,
    pub label: String,
    pub subtitle: String,
    pub sort_order: i32,
}

#[derive(Debug, Clone)]
pub struct IntegrationsSnapshot {
    pub categories: Vec<IntegrationCategory>,
    pub integrations: Vec<Integration>,
}

/// Catalog `connected` can lag the nested connection row. Either is enough.
/// An `error` row is not connected — the token was rejected.
/// Repo full names stored on a GitHub App connection. Empty when unknown.
pub fn granted_repos(conn: &ConnectionPublic) -> Vec<String> {
    conn.config
        .get("granted_repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub fn live_connections(i: &Integration) -> Vec<ConnectionPublic> {
    let rows = if i.connections.is_empty() {
        i.connection.clone().into_iter().collect::<Vec<_>>()
    } else {
        i.connections.clone()
    };
    rows.into_iter()
        .filter(|c| !c.status.eq_ignore_ascii_case("disconnected"))
        .collect()
}

pub fn integration_is_connected(i: &Integration) -> bool {
    live_connections(i).iter().any(|c| {
        !c.status.eq_ignore_ascii_case("error")
            && (c.connected || c.status.eq_ignore_ascii_case("connected") || i.connected)
    })
}

/// A saved login was rejected and no healthy connection remains.
pub fn integration_needs_reconnect(i: &Integration) -> bool {
    let rows = live_connections(i);
    !rows.is_empty() && rows.iter().all(|c| c.status.eq_ignore_ascii_case("error"))
}

/// Inbox and proxy errors that mean the saved login should be replaced.
pub fn message_is_auth_failure(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("invalid token")
        || lower.contains("invalid username")
        || lower.contains("bad credentials")
        || lower.contains("token_revoked")
        || lower.contains("invalid_auth")
        || lower.contains("token expired")
        || lower.contains("no longer valid")
}

pub fn find_integration<'a>(list: &'a [Integration], provider: &str) -> Option<&'a Integration> {
    list.iter()
        .find(|i| i.id.eq_ignore_ascii_case(provider.trim()))
}

#[derive(Debug, Clone, Deserialize)]
struct IntegrationListResponse {
    #[serde(default)]
    categories: Vec<IntegrationCategory>,
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
    Ok(load_integrations(workspace_id).await?.integrations)
}

pub async fn load_integrations(
    workspace_id: &str,
) -> Result<IntegrationsSnapshot, IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations");
    let res = authorized_get(&path).await?;
    let body: IntegrationListResponse = parse_json_or_error(res).await?;
    Ok(IntegrationsSnapshot {
        categories: body.categories,
        integrations: body.integrations,
    })
}

/// Absolute URL for full-page OAuth navigation (same-origin in dev via proxy).
pub fn oauth_start_url(workspace_id: &str, provider: &str) -> String {
    oauth_start_url_named(workspace_id, provider, "")
}

pub fn oauth_start_url_named(workspace_id: &str, provider: &str, name: &str) -> String {
    oauth_start_url_setup(workspace_id, provider, name, "")
}

pub fn oauth_start_url_setup(
    workspace_id: &str,
    provider: &str,
    name: &str,
    instance_url: &str,
) -> String {
    let mut url = api_url(&format!(
        "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"
    ));
    let mut query = Vec::new();
    let name = name.trim();
    if !name.is_empty() {
        query.push(format!("name={}", urlencoding_name(name)));
    }
    let instance_url = instance_url.trim();
    if !instance_url.is_empty() {
        query.push(format!("url={}", urlencoding_name(instance_url)));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

fn urlencoding_name(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub async fn rename_connection(
    workspace_id: &str,
    provider: &str,
    connection_id: &str,
    display_name: &str,
) -> Result<(), IntegrationsError> {
    let path = format!(
        "/api/workspaces/{workspace_id}/integrations/{provider}/connections/{connection_id}"
    );
    let res = authorized_patch_json(&path, &json!({ "display_name": display_name })).await?;
    if res.status().is_success() {
        return Ok(());
    }
    parse_json_or_error::<ErrorBody>(res).await?;
    Ok(())
}

pub async fn disconnect_connection(
    workspace_id: &str,
    provider: &str,
    connection_id: &str,
) -> Result<(), IntegrationsError> {
    let path = format!(
        "/api/workspaces/{workspace_id}/integrations/{provider}/connections/{connection_id}"
    );
    let res = authorized_delete(&path).await?;
    if res.status().is_success() {
        Ok(())
    } else {
        let _ = parse_json_or_error::<ErrorBody>(res).await?;
        Ok(())
    }
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

/// Gogs has no OAuth app. The server turns this login into an API token.
pub async fn connect_gogs(
    workspace_id: &str,
    base_url: &str,
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<(), IntegrationsError> {
    let path = format!("/api/workspaces/{workspace_id}/integrations/gogs/connect");
    let send = || {
        with_credentials(client().post(api_url(&path)).json(&json!({
            "base_url": base_url,
            "username": username,
            "password": password,
            "display_name": display_name
        })))
        .send()
    };
    let first = send()
        .await
        .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?;
    let res = if first.status().as_u16() == 401 {
        refresh_session()
            .await
            .map_err(|e| IntegrationsError::Message(e.message()))?;
        send()
            .await
            .map_err(|e| IntegrationsError::Network(format!("could not reach API: {e}")))?
    } else {
        first
    };
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
            category_id: "chat".into(),
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
                display_name: "Work".into(),
                connected: true,
                last_error: None,
                config: serde_json::json!({ "granted_repos": ["acme/app"] }),
                defaults: ConnectionDefaults::default(),
            }),
            connections: Vec::new(),
        };
        assert!(integration_is_connected(&i));
        assert!(!integration_needs_reconnect(&i));
        assert!(find_integration(std::slice::from_ref(&i), "Slack").is_some());
    }

    #[test]
    fn error_status_needs_reconnect_and_is_not_connected() {
        let mut i = Integration {
            id: "gitea".into(),
            name: "Gitea".into(),
            description: String::new(),
            category: "code".into(),
            category_label: "Source control".into(),
            category_id: "source_control".into(),
            availability: "available".into(),
            icon: "gitea".into(),
            docs_url: None,
            auth_method: "oauth".into(),
            oauth_configured: true,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: true,
            connection: Some(ConnectionPublic {
                id: "1".into(),
                workspace_id: "w".into(),
                provider: "gitea".into(),
                status: "error".into(),
                account_label: Some("fleet".into()),
                display_name: "Gitea".into(),
                connected: false,
                last_error: Some("invalid username, password or token (401)".into()),
                config: serde_json::json!({}),
                defaults: ConnectionDefaults::default(),
            }),
            connections: Vec::new(),
        };
        assert!(integration_needs_reconnect(&i));
        assert!(!integration_is_connected(&i));
        assert!(message_is_auth_failure(
            i.connection
                .as_ref()
                .unwrap()
                .last_error
                .as_deref()
                .unwrap()
        ));
        i.connection.as_mut().unwrap().status = "connected".into();
        i.connection.as_mut().unwrap().connected = true;
        assert!(!integration_needs_reconnect(&i));
        assert!(integration_is_connected(&i));
    }

    #[test]
    fn disconnected_without_row_is_not_connected() {
        let i = Integration {
            id: "slack".into(),
            name: "Slack".into(),
            description: String::new(),
            category: "hitl".into(),
            category_label: "HITL".into(),
            category_id: "chat".into(),
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
            connections: Vec::new(),
        };
        assert!(!integration_is_connected(&i));
    }

    #[test]
    fn granted_repos_reads_config_names() {
        let conn = ConnectionPublic {
            id: "1".into(),
            workspace_id: "w".into(),
            provider: "github".into(),
            status: "connected".into(),
            account_label: Some("vinybrun".into()),
            display_name: "GitHub".into(),
            connected: true,
            last_error: None,
            config: serde_json::json!({ "granted_repos": ["vinybrun/agent-fleet-test-repo", ""] }),
            defaults: ConnectionDefaults::default(),
        };
        assert_eq!(
            granted_repos(&conn),
            vec!["vinybrun/agent-fleet-test-repo".to_string()]
        );
    }
}
