//! Provider-specific authorize URLs and code → token exchange.

use fleet_auth::{AppError, AppResult};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{IntegrationsConfig, OAuthClientConfig};

#[derive(Debug, Clone)]
pub struct TokenBundle {
    /// Stored under credentials for the control plane / AI proxy.
    pub credentials: Value,
    /// Non-secret metadata (account, team, site, …).
    pub config: Value,
    pub account_label: Option<String>,
}

pub fn authorize_url(
    provider: &str,
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    state: &str,
) -> AppResult<String> {
    let redirect = cfg.callback_url(provider);
    let redirect_enc = urlencoding(redirect.as_str());
    let state_enc = urlencoding(state);
    let client_id = urlencoding(&client.client_id);

    let url = match provider {
        "github" => format!(
            "https://github.com/login/oauth/authorize?client_id={client_id}&redirect_uri={redirect_enc}&scope={scope}&state={state_enc}",
            scope = urlencoding("read:user repo"),
        ),
        "slack" => format!(
            "https://slack.com/oauth/v2/authorize?client_id={client_id}&scope={scope}&redirect_uri={redirect_enc}&state={state_enc}",
            scope = urlencoding("chat:write,channels:read,users:read"),
        ),
        "linear" => format!(
            "https://linear.app/oauth/authorize?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&scope={scope}&state={state_enc}&prompt=consent",
            scope = urlencoding("read,write"),
        ),
        "jira" => format!(
            "https://auth.atlassian.com/authorize?audience=api.atlassian.com&client_id={client_id}&scope={scope}&redirect_uri={redirect_enc}&state={state_enc}&response_type=code&prompt=consent",
            scope = urlencoding("read:jira-work write:jira-work read:jira-user offline_access"),
        ),
        _ => {
            return Err(AppError::bad_request(format!(
                "OAuth is not supported for {provider}"
            )));
        }
    };
    Ok(url)
}

pub async fn exchange_code(
    provider: &str,
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    match provider {
        "github" => exchange_github(cfg, client, code).await,
        "slack" => exchange_slack(cfg, client, code).await,
        "linear" => exchange_linear(cfg, client, code).await,
        "jira" => exchange_jira(cfg, client, code).await,
        _ => Err(AppError::bad_request(format!(
            "OAuth is not supported for {provider}"
        ))),
    }
}

async fn exchange_github(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .json(&json!({
            "client_id": client.client_id,
            "client_secret": client.client_secret,
            "code": code,
            "redirect_uri": cfg.callback_url("github"),
        }))
        .send()
        .await
        .map_err(|e| AppError::internal(format!("GitHub token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("GitHub token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("GitHub OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("GitHub did not return an access token"))?
        .to_string();
    let scope = body
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("bearer")
        .to_string();

    // Optional: resolve login for account_label
    let account_label = fetch_github_login(&http, &access_token).await;

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "token_type": token_type,
            "scope": scope,
        }),
        config: json!({ "provider": "github" }),
        account_label,
    })
}

async fn fetch_github_login(http: &reqwest::Client, token: &str) -> Option<String> {
    let res = http
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "agent-fleet")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    body.get("login")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn exchange_slack(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://slack.com/api/oauth.v2.access")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", &cfg.callback_url("slack")),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Slack token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Slack token parse failed: {e}")))?;

    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_error");
        return Err(AppError::bad_request(format!("Slack OAuth: {err}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Slack did not return an access token"))?
        .to_string();

    let team_name = body
        .pointer("/team/name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let team_id = body
        .pointer("/team/id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bot_user_id = body
        .get("bot_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "token_type": "bot",
            "bot_user_id": bot_user_id,
        }),
        config: json!({
            "provider": "slack",
            "team_id": team_id,
            "team_name": team_name,
        }),
        account_label: team_name,
    })
}

async fn exchange_linear(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://api.linear.app/oauth/token")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", &cfg.callback_url("linear")),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Linear token request failed: {e}")))?;

    #[derive(Deserialize)]
    struct LinearToken {
        access_token: String,
        #[serde(default)]
        token_type: Option<String>,
        #[serde(default)]
        scope: Option<Value>,
        #[serde(default)]
        expires_in: Option<u64>,
        #[serde(default)]
        error: Option<String>,
    }

    let body: LinearToken = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Linear token parse failed: {e}")))?;

    if let Some(err) = body.error {
        return Err(AppError::bad_request(format!("Linear OAuth: {err}")));
    }

    Ok(TokenBundle {
        credentials: json!({
            "access_token": body.access_token,
            "token_type": body.token_type.unwrap_or_else(|| "Bearer".into()),
            "scope": body.scope,
            "expires_in": body.expires_in,
        }),
        config: json!({ "provider": "linear" }),
        account_label: Some("Linear".into()),
    })
}

async fn exchange_jira(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://auth.atlassian.com/oauth/token")
        .json(&json!({
            "grant_type": "authorization_code",
            "client_id": client.client_id,
            "client_secret": client.client_secret,
            "code": code,
            "redirect_uri": cfg.callback_url("jira"),
        }))
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Jira token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Jira token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Jira OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Jira did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let scope = body.get("scope").cloned();

    // Resolve cloud id for later API calls
    let site = fetch_atlassian_site(&http, &access_token).await;
    let (cloud_id, site_name) = match site {
        Some((id, name)) => (Some(id), Some(name)),
        None => (None, None),
    };
    let account_label = site_name.clone().or_else(|| Some("Jira".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": "Bearer",
            "expires_in": expires_in,
            "scope": scope,
        }),
        config: json!({
            "provider": "jira",
            "cloud_id": cloud_id,
            "site_name": site_name,
        }),
        account_label,
    })
}

async fn fetch_atlassian_site(
    http: &reqwest::Client,
    token: &str,
) -> Option<(String, String)> {
    let res = http
        .get("https://api.atlassian.com/oauth/token/accessible-resources")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    let first = body.as_array()?.first()?;
    let id = first.get("id")?.as_str()?.to_string();
    let name = first
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Jira")
        .to_string();
    Some((id, name))
}

fn http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::internal(format!("http client: {e}")))
}

fn urlencoding(s: &str) -> String {
    // Minimal form-encoding for query values.
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}
