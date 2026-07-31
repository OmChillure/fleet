//! OAuth app credentials (Fleet's identity at each platform).
//!
//! Per-user/workspace tokens from OAuth live in Postgres — not here.

use std::env;

use anyhow::bail;

/// One OAuth application's client credentials.
#[derive(Clone, Debug)]
pub struct OAuthClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Integrations + OAuth settings for the control plane.
#[derive(Clone, Debug)]
pub struct IntegrationsConfig {
    /// Public base URL of fleet-server (used as OAuth redirect_uri origin).
    /// e.g. `http://localhost:8080`
    pub api_public_base: String,
    /// Where to send the browser after OAuth (fleet-ui).
    /// e.g. `http://localhost:3030`
    pub ui_public_base: String,
    /// Same secret as auth — signs OAuth `state` JWTs.
    pub state_secret: String,

    pub github: Option<OAuthClientConfig>,
    pub slack: Option<OAuthClientConfig>,
    pub linear: Option<OAuthClientConfig>,
    pub jira: Option<OAuthClientConfig>,
}

impl IntegrationsConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_public_base = env::var("API_PUBLIC_BASE")
            .unwrap_or_else(|_| "http://localhost:8080".into())
            .trim_end_matches('/')
            .to_string();
        let ui_public_base = env::var("UI_PUBLIC_BASE")
            .unwrap_or_else(|_| "http://localhost:3030".into())
            .trim_end_matches('/')
            .to_string();

        let state_secret = env::var("JWT_SECRET").unwrap_or_else(|_| {
            "dev-change-me-to-a-long-random-secret-at-least-32-chars".into()
        });
        if state_secret.len() < 32 {
            bail!("JWT_SECRET must be at least 32 characters (also used for OAuth state)");
        }

        Ok(Self {
            api_public_base,
            ui_public_base,
            state_secret,
            github: optional_oauth("GITHUB_CLIENT_ID", "GITHUB_CLIENT_SECRET")?,
            slack: optional_oauth("SLACK_CLIENT_ID", "SLACK_CLIENT_SECRET")?,
            linear: optional_oauth("LINEAR_CLIENT_ID", "LINEAR_CLIENT_SECRET")?,
            jira: optional_oauth("JIRA_CLIENT_ID", "JIRA_CLIENT_SECRET")?,
        })
    }

    pub fn oauth_for(&self, provider: &str) -> Option<&OAuthClientConfig> {
        match provider {
            "github" => self.github.as_ref(),
            "slack" => self.slack.as_ref(),
            "linear" => self.linear.as_ref(),
            "jira" => self.jira.as_ref(),
            _ => None,
        }
    }

    pub fn is_oauth_configured(&self, provider: &str) -> bool {
        self.oauth_for(provider).is_some()
    }

    /// e.g. `http://localhost:8080/api/integrations/oauth/callback/github`
    pub fn callback_url(&self, provider: &str) -> String {
        format!(
            "{}/api/integrations/oauth/callback/{}",
            self.api_public_base, provider
        )
    }

    /// After success/error, land on the integrations page.
    pub fn ui_integrations_url(&self, workspace_id: &str, query: &str) -> String {
        let q = if query.is_empty() {
            String::new()
        } else if query.starts_with('?') {
            query.to_string()
        } else {
            format!("?{query}")
        };
        format!(
            "{}/{}/integrations{}",
            self.ui_public_base, workspace_id, q
        )
    }
}

fn optional_oauth(id_key: &str, secret_key: &str) -> anyhow::Result<Option<OAuthClientConfig>> {
    let id = env::var(id_key).ok().filter(|s| !s.trim().is_empty());
    let secret = env::var(secret_key).ok().filter(|s| !s.trim().is_empty());
    match (id, secret) {
        (Some(client_id), Some(client_secret)) => Ok(Some(OAuthClientConfig {
            client_id: client_id.trim().into(),
            client_secret: client_secret.trim().into(),
        })),
        (None, None) => Ok(None),
        (Some(_), None) => bail!("{secret_key} is required when {id_key} is set"),
        (None, Some(_)) => bail!("{id_key} is required when {secret_key} is set"),
    }
}
