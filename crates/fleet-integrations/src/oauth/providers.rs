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
        "github" => {
            if let Some(slug) = client
                .github_app_slug
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                crate::oauth::github_install_url(slug, state)
            } else {
                format!(
                    "https://github.com/login/oauth/authorize?client_id={client_id}&redirect_uri={redirect_enc}&scope={scope}&state={state_enc}",
                    scope = urlencoding("read:user repo"),
                )
            }
        }
        "gitlab" => format!(
            "https://gitlab.com/oauth/authorize?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&scope={scope}&state={state_enc}",
            scope = urlencoding("api"),
        ),
        "slack" => format!(
            "https://slack.com/oauth/v2/authorize?client_id={client_id}&scope={scope}&redirect_uri={redirect_enc}&state={state_enc}",
            scope = urlencoding(
                "chat:write,channels:read,channels:history,groups:read,groups:history,users:read,im:read,im:write,mpim:read"
            ),
        ),
        "linear" => format!(
            "https://linear.app/oauth/authorize?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&scope={scope}&state={state_enc}&prompt=consent",
            scope = urlencoding("read,write"),
        ),
        "jira" => format!(
            "https://auth.atlassian.com/authorize?audience=api.atlassian.com&client_id={client_id}&scope={scope}&redirect_uri={redirect_enc}&state={state_enc}&response_type=code&prompt=consent",
            scope = urlencoding("read:jira-work write:jira-work read:jira-user offline_access"),
        ),
        "figma" => format!(
            "https://www.figma.com/oauth?client_id={client_id}&redirect_uri={redirect_enc}&scope={scope}&state={state_enc}&response_type=code",
            scope = urlencoding("file_read"),
        ),
        "notion" => format!(
            "https://api.notion.com/v1/oauth/authorize?client_id={client_id}&response_type=code&owner=user&redirect_uri={redirect_enc}&state={state_enc}",
        ),
        "google_drive" => format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&scope={scope}&state={state_enc}&access_type=offline&prompt=consent",
            scope = urlencoding("https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/userinfo.email"),
        ),
        "teams" => format!(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id={client_id}&response_type=code&redirect_uri={redirect_enc}&response_mode=query&scope={scope}&state={state_enc}",
            scope = urlencoding("offline_access User.Read Chat.ReadWrite ChannelMessage.Send Team.ReadBasic.All"),
        ),
        "discord" => format!(
            "https://discord.com/api/oauth2/authorize?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&scope={scope}&state={state_enc}",
            scope = urlencoding("identify guilds"),
        ),
        "asana" => format!(
            "https://app.asana.com/-/oauth_authorize?client_id={client_id}&redirect_uri={redirect_enc}&response_type=code&state={state_enc}&scope={scope}",
            scope = urlencoding(
                "workspaces:read projects:read projects:write tasks:read tasks:write stories:read stories:write users:read"
            ),
        ),
        "trello" => {
            // Trello's authorize flow returns the user token in the URL fragment.
            // Callback HTML lifts `#token=` into `?code=` for the shared exchange path.
            let return_url = format!("{}?state={state_enc}", cfg.callback_url("trello"));
            format!(
                "https://trello.com/1/authorize?expiration=never&name={name}&scope={scope}&response_type=token&key={client_id}&return_url={return_enc}&callback_method=fragment",
                name = urlencoding("Agent Fleet"),
                scope = urlencoding("read,write,account"),
                return_enc = urlencoding(&return_url),
            )
        }
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
    tracing::debug!(provider = %provider, "oauth token exchange starting");
    let result = match provider {
        "github" => exchange_github(cfg, client, code).await,
        "gitlab" => exchange_gitlab(cfg, client, code).await,
        "slack" => exchange_slack(cfg, client, code).await,
        "linear" => exchange_linear(cfg, client, code).await,
        "jira" => exchange_jira(cfg, client, code).await,
        "figma" => exchange_figma(cfg, client, code).await,
        "notion" => exchange_notion(cfg, client, code).await,
        "google_drive" => exchange_google_drive(cfg, client, code).await,
        "teams" => exchange_teams(cfg, client, code).await,
        "discord" => exchange_discord(cfg, client, code).await,
        "trello" => exchange_trello(client, code).await,
        "asana" => exchange_asana(cfg, client, code).await,
        _ => Err(AppError::bad_request(format!(
            "OAuth is not supported for {provider}"
        ))),
    };
    match &result {
        Ok(_) => tracing::info!(provider = %provider, "oauth token exchange succeeded"),
        Err(e) => tracing::error!(
            provider = %provider,
            error = %e,
            "oauth token exchange failed"
        ),
    }
    result
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

async fn exchange_gitlab(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://gitlab.com/oauth/token")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &cfg.callback_url("gitlab")),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("GitLab token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("GitLab token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("GitLab OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("GitLab did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let scope = body.get("scope").cloned();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();

    let account_label = fetch_gitlab_username(&http, &access_token)
        .await
        .or_else(|| Some("GitLab".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": token_type,
            "expires_in": expires_in,
            "scope": scope,
        }),
        config: json!({ "provider": "gitlab" }),
        account_label,
    })
}

async fn fetch_gitlab_username(http: &reqwest::Client, token: &str) -> Option<String> {
    let res = http
        .get("https://gitlab.com/api/v4/user")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    body.get("username")
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

async fn exchange_figma(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://api.figma.com/v1/oauth/token")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("redirect_uri", &cfg.callback_url("figma")),
            ("code", code),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Figma token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Figma token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("message")
            .or_else(|| body.get("error_description"))
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Figma OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Figma did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let user_id = body
        .get("user_id_string")
        .or_else(|| body.get("user_id"))
        .cloned();

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": "Bearer",
            "expires_in": expires_in,
        }),
        config: json!({
            "provider": "figma",
            "user_id": user_id,
        }),
        account_label: Some("Figma".into()),
    })
}

async fn exchange_notion(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://api.notion.com/v1/oauth/token")
        .basic_auth(&client.client_id, Some(&client.client_secret))
        .header("Notion-Version", "2022-06-28")
        .json(&json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": cfg.callback_url("notion"),
        }))
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Notion token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Notion token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Notion OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Notion did not return an access token"))?
        .to_string();

    let workspace_name = body
        .get("workspace_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let workspace_id = body.get("workspace_id").cloned();
    let bot_id = body.get("bot_id").cloned();
    let owner = body.get("owner").cloned();

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "token_type": body.get("token_type").cloned().unwrap_or(json!("bearer")),
            "bot_id": bot_id,
        }),
        config: json!({
            "provider": "notion",
            "workspace_id": workspace_id,
            "workspace_name": workspace_name,
            "owner": owner,
        }),
        account_label: workspace_name.or_else(|| Some("Notion".into())),
    })
}

async fn exchange_google_drive(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", &cfg.callback_url("google_drive")),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Google token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Google token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Google Drive OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Google did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let scope = body.get("scope").cloned();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();

    let account_label = fetch_google_email(&http, &access_token)
        .await
        .or_else(|| Some("Google Drive".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": token_type,
            "expires_in": expires_in,
            "scope": scope,
        }),
        config: json!({ "provider": "google_drive" }),
        account_label,
    })
}

async fn fetch_google_email(http: &reqwest::Client, token: &str) -> Option<String> {
    let res = http
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    body.get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn exchange_teams(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://login.microsoftonline.com/common/oauth2/v2.0/token")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", &cfg.callback_url("teams")),
            ("grant_type", "authorization_code"),
            (
                "scope",
                "offline_access User.Read Chat.ReadWrite ChannelMessage.Send Team.ReadBasic.All",
            ),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Teams token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Teams token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Teams OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Teams did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let scope = body.get("scope").cloned();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();

    let account_label = fetch_microsoft_display_name(&http, &access_token)
        .await
        .or_else(|| Some("Microsoft Teams".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": token_type,
            "expires_in": expires_in,
            "scope": scope,
        }),
        config: json!({ "provider": "teams" }),
        account_label,
    })
}

async fn fetch_microsoft_display_name(http: &reqwest::Client, token: &str) -> Option<String> {
    let res = http
        .get("https://graph.microsoft.com/v1.0/me")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    body.get("displayName")
        .or_else(|| body.get("userPrincipalName"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn exchange_discord(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://discord.com/api/oauth2/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &cfg.callback_url("discord")),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Discord token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Discord token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Discord OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Discord did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let scope = body.get("scope").cloned();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();

    let account_label = fetch_discord_username(&http, &access_token, &token_type)
        .await
        .or_else(|| Some("Discord".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": token_type,
            "expires_in": expires_in,
            "scope": scope,
        }),
        config: json!({ "provider": "discord" }),
        account_label,
    })
}

async fn fetch_discord_username(
    http: &reqwest::Client,
    token: &str,
    token_type: &str,
) -> Option<String> {
    let res = http
        .get("https://discord.com/api/users/@me")
        .header("Authorization", format!("{token_type} {token}"))
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    let username = body.get("username").and_then(|v| v.as_str())?;
    let global = body
        .get("global_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    Some(global.unwrap_or(username).to_string())
}

async fn exchange_asana(
    cfg: &IntegrationsConfig,
    client: &OAuthClientConfig,
    code: &str,
) -> AppResult<TokenBundle> {
    let http = http_client()?;
    let res = http
        .post("https://app.asana.com/-/oauth_token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("redirect_uri", &cfg.callback_url("asana")),
            ("code", code),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("Asana token request failed: {e}")))?;

    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("Asana token parse failed: {e}")))?;

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or(err);
        return Err(AppError::bad_request(format!("Asana OAuth: {desc}")));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Asana did not return an access token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").cloned();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();

    let account_label = body
        .pointer("/data/name")
        .or_else(|| body.pointer("/data/email"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some("Asana".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": token_type,
            "expires_in": expires_in,
        }),
        config: json!({ "provider": "asana" }),
        account_label,
    })
}

async fn exchange_trello(client: &OAuthClientConfig, code: &str) -> AppResult<TokenBundle> {
    // Trello authorize returns the user token directly (no code exchange).
    let access_token = code.trim();
    if access_token.is_empty() {
        return Err(AppError::bad_request("Trello did not return a token"));
    }
    let http = http_client()?;
    let account_label = fetch_trello_username(&http, &client.client_id, access_token)
        .await
        .or_else(|| Some("Trello".into()));

    Ok(TokenBundle {
        credentials: json!({
            "access_token": access_token,
            "token_type": "trello",
        }),
        config: json!({
            "provider": "trello",
            "api_key": client.client_id,
        }),
        account_label,
    })
}

async fn fetch_trello_username(
    http: &reqwest::Client,
    api_key: &str,
    token: &str,
) -> Option<String> {
    let res = http
        .get("https://api.trello.com/1/members/me")
        .query(&[("key", api_key), ("token", token), ("fields", "username,fullName")])
        .send()
        .await
        .ok()?;
    let body: Value = res.json().await.ok()?;
    body.get("username")
        .or_else(|| body.get("fullName"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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
