//! OAuth app credentials (Fleet's identity at each platform).
//!
//! Per-user/workspace tokens from OAuth live in Postgres — encrypted at rest
//! with [`CredentialsKey`] (see `crypto`).

use std::env;

use anyhow::bail;

use crate::crypto::CredentialsKey;

/// One OAuth application's client credentials.
#[derive(Clone, Debug)]
pub struct OAuthClientConfig {
    pub client_id: String,
    pub client_secret: String,
    /// When set, GitHub connect uses the App install URL (repo picker + permissions).
    pub github_app_slug: Option<String>,
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
    /// AES-256 key for encrypting OAuth tokens in Postgres.
    pub credentials_key: CredentialsKey,

    pub github: Option<OAuthClientConfig>,
    pub gitlab: Option<OAuthClientConfig>,
    pub slack: Option<OAuthClientConfig>,
    pub linear: Option<OAuthClientConfig>,
    pub jira: Option<OAuthClientConfig>,
    pub figma: Option<OAuthClientConfig>,
    pub notion: Option<OAuthClientConfig>,
    pub google_drive: Option<OAuthClientConfig>,
    pub teams: Option<OAuthClientConfig>,
    pub discord: Option<OAuthClientConfig>,
    pub trello: Option<OAuthClientConfig>,
    pub asana: Option<OAuthClientConfig>,
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

        let credentials_key = load_credentials_key(&state_secret)?;

        Ok(Self {
            api_public_base,
            ui_public_base,
            state_secret,
            credentials_key,
            github: optional_oauth("GITHUB_CLIENT_ID", "GITHUB_CLIENT_SECRET")?,
            gitlab: optional_oauth("GITLAB_CLIENT_ID", "GITLAB_CLIENT_SECRET")?,
            slack: optional_oauth("SLACK_CLIENT_ID", "SLACK_CLIENT_SECRET")?,
            linear: optional_oauth("LINEAR_CLIENT_ID", "LINEAR_CLIENT_SECRET")?,
            jira: optional_oauth("JIRA_CLIENT_ID", "JIRA_CLIENT_SECRET")?,
            figma: optional_oauth("FIGMA_CLIENT_ID", "FIGMA_CLIENT_SECRET")?,
            notion: optional_oauth("NOTION_CLIENT_ID", "NOTION_CLIENT_SECRET")?,
            google_drive: optional_oauth("GOOGLE_DRIVE_CLIENT_ID", "GOOGLE_DRIVE_CLIENT_SECRET")?,
            teams: optional_oauth("TEAMS_CLIENT_ID", "TEAMS_CLIENT_SECRET")?,
            discord: optional_oauth("DISCORD_CLIENT_ID", "DISCORD_CLIENT_SECRET")?,
            trello: optional_oauth("TRELLO_CLIENT_ID", "TRELLO_CLIENT_SECRET")?,
            asana: optional_oauth("ASANA_CLIENT_ID", "ASANA_CLIENT_SECRET")?,
        })
    }

    pub fn oauth_for(&self, provider: &str) -> Option<&OAuthClientConfig> {
        match provider {
            "github" => self.github.as_ref(),
            "gitlab" => self.gitlab.as_ref(),
            "slack" => self.slack.as_ref(),
            "linear" => self.linear.as_ref(),
            "jira" => self.jira.as_ref(),
            "figma" => self.figma.as_ref(),
            "notion" => self.notion.as_ref(),
            "google_drive" => self.google_drive.as_ref(),
            "teams" => self.teams.as_ref(),
            "discord" => self.discord.as_ref(),
            "trello" => self.trello.as_ref(),
            "asana" => self.asana.as_ref(),
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

    /// After success/error, land on the integrations page (legacy / full page).
    pub fn ui_integrations_url(&self, workspace_id: &str, query: &str) -> String {
        let q = format_query(query);
        format!(
            "{}/{}/integrations{}",
            self.ui_public_base, workspace_id, q
        )
    }

    /// Minimal popup page after OAuth — signals the original tab and closes.
    /// Does **not** reopen the full app UI in the auth tab.
    pub fn ui_oauth_done_url(&self, query: &str) -> String {
        let q = format_query(query);
        format!("{}/oauth/done{}", self.ui_public_base, q)
    }
}

fn format_query(query: &str) -> String {
    if query.is_empty() {
        String::new()
    } else if query.starts_with('?') {
        query.to_string()
    } else {
        format!("?{query}")
    }
}

fn optional_oauth(id_key: &str, secret_key: &str) -> anyhow::Result<Option<OAuthClientConfig>> {
    let id = env::var(id_key).ok().filter(|s| !s.trim().is_empty());
    let secret = env::var(secret_key).ok().filter(|s| !s.trim().is_empty());
    match (id, secret) {
        (Some(client_id), Some(client_secret)) => Ok(Some(OAuthClientConfig {
            client_id: client_id.trim().into(),
            client_secret: client_secret.trim().into(),
            github_app_slug: None,
        })),
        (None, None) => Ok(None),
        (Some(_), None) => bail!("{secret_key} is required when {id_key} is set"),
        (None, Some(_)) => bail!("{id_key} is required when {secret_key} is set"),
    }
}

/// Prefer dedicated `INTEGRATIONS_ENCRYPTION_KEY`; fall back to `JWT_SECRET` in
/// dev so a single secret still encrypts tokens (with a warning).
fn load_credentials_key(jwt_secret: &str) -> anyhow::Result<CredentialsKey> {
    match env::var("INTEGRATIONS_ENCRYPTION_KEY") {
        Ok(s) if !s.trim().is_empty() => CredentialsKey::from_secret(&s),
        _ => {
            tracing::warn!(
                "INTEGRATIONS_ENCRYPTION_KEY not set; deriving AES key from JWT_SECRET. \
                 Set a dedicated key in production (`openssl rand -hex 32`)."
            );
            CredentialsKey::from_secret(jwt_secret)
        }
    }
}
