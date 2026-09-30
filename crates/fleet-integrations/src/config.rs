//! OAuth app credentials (Fleet's identity at each platform).
//!
//! Per-user/workspace tokens from OAuth live in Postgres — encrypted at rest
//! with [`CredentialsKey`] (see `crypto`).

use std::env;

use anyhow::bail;

use crate::crypto::CredentialsKey;

fn env_port(keys: &[&str], default: u16) -> u16 {
    keys.iter()
        .find_map(|k| env::var(k).ok().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(default)
}

fn env_url_or_localhost(key: &str, port: u16) -> String {
    env::var(key)
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .map(|url| rewrite_loopback_port(&url, port).unwrap_or(url))
        .unwrap_or_else(|| format!("http://localhost:{port}"))
}

/// Loopback `http(s)://localhost|127.0.0.1|[::1][:old]` follows `port`.
/// A public hostname is left unchanged (VPS `https://agentfleet.studio`).
fn rewrite_loopback_port(url: &str, port: u16) -> Option<String> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else {
        return None;
    };
    let (hostport, path) = rest
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((rest, String::new()));
    let host = hostport.split(':').next().unwrap_or(hostport);
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    Some(format!("{scheme}://{host}:{port}{path}"))
}

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
    pub gitea: Option<OAuthClientConfig>,
    /// Root of the Gitea instance (`http://localhost:3033`), no trailing slash.
    pub gitea_base_url: String,
    /// Root of the Gogs instance (`http://localhost:3034`), no trailing slash.
    /// Gogs has no OAuth apps; Connect exchanges a login for an API token.
    pub gogs_base_url: String,
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
    pub bitbucket: Option<OAuthClientConfig>,
    pub confluence: Option<OAuthClientConfig>,
}

impl IntegrationsConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        // Unset public bases follow listen ports so OAuth is not pinned to a dead :8080.
        let api_public_base = env_url_or_localhost(
            "API_PUBLIC_BASE",
            env_port(&["PORT", "FLEET_API_PORT", "AUTH_PORT"], 8080),
        );
        let ui_public_base =
            env_url_or_localhost("UI_PUBLIC_BASE", env_port(&["FLEET_UI_PORT"], 3030));

        let state_secret = match env::var("JWT_SECRET") {
            Ok(s) => {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    bail!(
                        "JWT_SECRET is required and must not be empty or whitespace-only \
                         (also used for OAuth state)"
                    );
                }
                trimmed
            }
            _ => bail!(
                "JWT_SECRET is required and must not be empty or whitespace-only \
                 (also used for OAuth state)"
            ),
        };
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
            gitea: optional_oauth("GITEA_CLIENT_ID", "GITEA_CLIENT_SECRET")?,
            gitea_base_url: env::var("GITEA_BASE_URL")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "http://localhost:3033".into()),
            gogs_base_url: env::var("GOGS_BASE_URL")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "http://localhost:3034".into()),
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
            bitbucket: optional_oauth("BITBUCKET_CLIENT_ID", "BITBUCKET_CLIENT_SECRET")?,
            confluence: optional_oauth("CONFLUENCE_CLIENT_ID", "CONFLUENCE_CLIENT_SECRET")?,
        })
    }

    pub fn oauth_for(&self, provider: &str) -> Option<&OAuthClientConfig> {
        match provider {
            "github" => self.github.as_ref(),
            "gitlab" => self.gitlab.as_ref(),
            "gitea" => self.gitea.as_ref(),
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
            "bitbucket" => self.bitbucket.as_ref(),
            "confluence" => self.confluence.as_ref(),
            _ => None,
        }
    }

    pub fn is_oauth_configured(&self, provider: &str) -> bool {
        if provider == "gogs" {
            return !self.gogs_base_url.trim().is_empty();
        }
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
        format!("{}/{}/integrations{}", self.ui_public_base, workspace_id, q)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Serializes env mutation in this module so parallel tests do not race.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_jwt_secret<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let previous = env::var("JWT_SECRET").ok();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            match value {
                Some(v) => env::set_var("JWT_SECRET", v),
                None => env::remove_var("JWT_SECRET"),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            match previous {
                Some(v) => env::set_var("JWT_SECRET", v),
                None => env::remove_var("JWT_SECRET"),
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    const PUBLIC_DEFAULT: &str = "dev-change-me-to-a-long-random-secret-at-least-32-chars";

    fn assert_jwt_secret_rejected(result: anyhow::Result<IntegrationsConfig>, case: &str) {
        match result {
            Ok(cfg) => panic!(
                "{case}: JWT_SECRET must fail from_env; must not boot with the public default \
                 (got state_secret==public_default={}, len={})",
                cfg.state_secret == PUBLIC_DEFAULT,
                cfg.state_secret.len()
            ),
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("JWT_SECRET"),
                    "{case}: error must mention JWT_SECRET, got {msg}"
                );
            }
        }
    }

    #[test]
    fn jwt_secret_unset_and_whitespace_fails_from_env() {
        // Unset must not unwrap_or the published default (anyone can forge oauth_state).
        assert_jwt_secret_rejected(with_jwt_secret(None, IntegrationsConfig::from_env), "unset");
        for (case, value) in [
            ("empty", ""),
            ("spaces", "   "),
            ("tabs_newlines", "\t\n"),
            // 32 spaces pass the length gate today; whitespace-only is still not a secret.
            ("thirty_two_spaces", "                                "),
        ] {
            assert_jwt_secret_rejected(
                with_jwt_secret(Some(value), IntegrationsConfig::from_env),
                case,
            );
        }
    }

    #[test]
    fn jwt_secret_shorter_than_32_fails_from_env() {
        let err = with_jwt_secret(Some("too-short"), || {
            IntegrationsConfig::from_env().expect_err("short JWT_SECRET must fail")
        });
        let msg = format!("{err:#}");
        assert!(
            msg.contains("JWT_SECRET") && msg.contains("32"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn valid_secret_from_env_works() {
        let secret = "valid-secret-at-least-32-characters-long!";
        let cfg = with_jwt_secret(Some(secret), || {
            IntegrationsConfig::from_env().expect("valid JWT_SECRET")
        });
        assert_eq!(cfg.state_secret, secret);
        assert_ne!(cfg.state_secret, PUBLIC_DEFAULT);
    }

    #[test]
    fn unset_public_bases_follow_listen_ports() {
        let _guard = env_lock().lock().expect("env lock");
        let keys = [
            "API_PUBLIC_BASE",
            "UI_PUBLIC_BASE",
            "PORT",
            "FLEET_API_PORT",
            "AUTH_PORT",
            "FLEET_UI_PORT",
            "JWT_SECRET",
        ];
        let prev: Vec<(&str, Option<String>)> =
            keys.iter().map(|k| (*k, env::var(k).ok())).collect();
        unsafe {
            for k in keys {
                env::remove_var(k);
            }
            env::set_var("PORT", "18280");
            env::set_var("FLEET_UI_PORT", "13230");
            env::set_var("JWT_SECRET", "valid-secret-at-least-32-characters-long!");
        }
        let cfg = IntegrationsConfig::from_env();
        unsafe {
            for (k, v) in prev {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let cfg = cfg.expect("from_env");
        assert_eq!(cfg.api_public_base, "http://localhost:18280");
        assert_eq!(cfg.ui_public_base, "http://localhost:13230");
    }

    #[test]
    fn loopback_public_bases_follow_listen_ports_public_hosts_stay() {
        let _guard = env_lock().lock().expect("env lock");
        let keys = [
            "API_PUBLIC_BASE",
            "UI_PUBLIC_BASE",
            "PORT",
            "FLEET_API_PORT",
            "AUTH_PORT",
            "FLEET_UI_PORT",
            "JWT_SECRET",
        ];
        let prev: Vec<(&str, Option<String>)> =
            keys.iter().map(|k| (*k, env::var(k).ok())).collect();
        unsafe {
            for k in keys {
                env::remove_var(k);
            }
            env::set_var("PORT", "18080");
            env::set_var("FLEET_UI_PORT", "13030");
            env::set_var("API_PUBLIC_BASE", "http://localhost:8080");
            env::set_var("UI_PUBLIC_BASE", "http://127.0.0.1:3030");
            env::set_var("JWT_SECRET", "valid-secret-at-least-32-characters-long!");
        }
        let loopback = IntegrationsConfig::from_env();
        unsafe {
            env::set_var("API_PUBLIC_BASE", "https://agentfleet.studio");
            env::set_var("UI_PUBLIC_BASE", "https://agentfleet.studio");
        }
        let public = IntegrationsConfig::from_env();
        unsafe {
            for (k, v) in prev {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let loopback = loopback.expect("loopback rewrite");
        assert_eq!(loopback.api_public_base, "http://localhost:18080");
        assert_eq!(loopback.ui_public_base, "http://127.0.0.1:13030");
        let public = public.expect("public host stays");
        assert_eq!(public.api_public_base, "https://agentfleet.studio");
        assert_eq!(public.ui_public_base, "https://agentfleet.studio");
    }

    #[test]
    fn jwt_secret_is_trimmed_before_length_check() {
        let secret = "valid-secret-at-least-32-characters-long!";
        let padded = format!("  {secret}  ");
        assert!(padded.len() > secret.len());
        let cfg = with_jwt_secret(Some(&padded), || {
            IntegrationsConfig::from_env().expect("padded JWT_SECRET must trim")
        });
        assert_eq!(cfg.state_secret, secret);

        // Untrimmed length ≥ 32 must not pass if the trimmed secret is short.
        let core = "short-under-32-chars!!";
        let padded_short = format!("     {core}     ");
        assert!(padded_short.len() >= 32);
        assert!(padded_short.trim().len() < 32);
        assert_jwt_secret_rejected(
            with_jwt_secret(Some(&padded_short), IntegrationsConfig::from_env),
            "padded_short",
        );
    }

    /// JWT_SECRET + INTEGRATIONS_ENCRYPTION_KEY under the same env_lock; both restored.
    fn with_credentials_env<T>(
        jwt_secret: &str,
        integrations_key: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let prev_jwt = env::var("JWT_SECRET").ok();
        let prev_iek = env::var("INTEGRATIONS_ENCRYPTION_KEY").ok();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            env::set_var("JWT_SECRET", jwt_secret);
            match integrations_key {
                Some(v) => env::set_var("INTEGRATIONS_ENCRYPTION_KEY", v),
                None => env::remove_var("INTEGRATIONS_ENCRYPTION_KEY"),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            match prev_jwt {
                Some(v) => env::set_var("JWT_SECRET", v),
                None => env::remove_var("JWT_SECRET"),
            }
            match prev_iek {
                Some(v) => env::set_var("INTEGRATIONS_ENCRYPTION_KEY", v),
                None => env::remove_var("INTEGRATIONS_ENCRYPTION_KEY"),
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn credentials_keys_match(a: &CredentialsKey, b: &CredentialsKey) -> bool {
        let probe = serde_json::json!({"probe": "iek"});
        a.encrypt(&probe)
            .ok()
            .and_then(|sealed| b.decrypt(&sealed).ok())
            .as_ref()
            == Some(&probe)
    }

    /// Distinct from list-jwt-secret-unset-fails-closed (JWT is set and valid).
    const IEK_JWT: &str = "iek-jwt-unique-at-least-32-chars-xxxx";
    const IEK_THIRTY_ONE: &str = "iek-thirty-one-chars-exactly-31";
    const IEK_PASSPHRASE: &str = "iek-enc-passphrase-not-hex-32ch!!";
    const IEK_HEX64: &str = "cafebabefacefeed0123456789abcdefcafebabefacefeed0123456789abcdef";

    fn assert_integrations_key_rejected(
        result: anyhow::Result<IntegrationsConfig>,
        jwt_secret: &str,
        integrations_key: &str,
        case: &str,
        require_32: bool,
    ) {
        match result {
            Ok(cfg) => {
                let jwt_key = CredentialsKey::from_secret(jwt_secret)
                    .expect("test JWT_SECRET must itself be a valid credentials secret");
                let fell_back = credentials_keys_match(&cfg.credentials_key, &jwt_key);
                panic!(
                    "{case}: INTEGRATIONS_ENCRYPTION_KEY must fail from_env; \
                     must not return Ok with a key derived from JWT_SECRET \
                     (fell_back_to_jwt={fell_back})"
                );
            }
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    !msg.contains(jwt_secret),
                    "{case}: error must not print JWT_SECRET"
                );
                assert!(
                    !msg.contains(integrations_key),
                    "{case}: error must not print the key"
                );
                assert!(
                    msg.contains("INTEGRATIONS_ENCRYPTION_KEY"),
                    "{case}: error must mention INTEGRATIONS_ENCRYPTION_KEY, got {msg}"
                );
                if require_32 {
                    assert!(
                        msg.contains("32"),
                        "{case}: error must mention 32, got {msg}"
                    );
                }
            }
        }
    }

    fn assert_dedicated_key(
        cfg: &IntegrationsConfig,
        dedicated: &str,
        jwt_secret: &str,
        case: &str,
    ) {
        let dedicated_key = CredentialsKey::from_secret(dedicated).unwrap_or_else(|e| {
            panic!("{case}: dedicated INTEGRATIONS_ENCRYPTION_KEY must itself parse: {e:#}")
        });
        let jwt_key = CredentialsKey::from_secret(jwt_secret)
            .unwrap_or_else(|e| panic!("{case}: jwt key: {e:#}"));
        assert!(
            credentials_keys_match(&cfg.credentials_key, &dedicated_key),
            "{case}: credentials_key must be the dedicated secret, not JWT_SECRET"
        );
        assert!(
            !credentials_keys_match(&cfg.credentials_key, &jwt_key),
            "{case}: dedicated key must differ from JWT_SECRET"
        );
    }

    #[test]
    fn integrations_encryption_key_short_fails_from_env() {
        assert!(IEK_JWT.len() >= 32);
        assert_eq!(IEK_THIRTY_ONE.len(), 31);
        for (case, value) in [("short", "short"), ("thirty_one", IEK_THIRTY_ONE)] {
            assert_integrations_key_rejected(
                with_credentials_env(IEK_JWT, Some(value), IntegrationsConfig::from_env),
                IEK_JWT,
                value,
                case,
                true,
            );
        }
    }

    #[test]
    fn integrations_encryption_key_from_env_uses_dedicated() {
        assert!(IEK_JWT.len() >= 32);
        assert!(IEK_PASSPHRASE.len() >= 32);
        assert!(IEK_PASSPHRASE.chars().any(|c| !c.is_ascii_hexdigit()));
        assert_eq!(IEK_HEX64.len(), 64);
        assert!(IEK_HEX64.chars().all(|c| c.is_ascii_hexdigit()));

        // Dedicated ≥32 non-hex passphrase → SHA-256 derive; not the JWT key.
        let passphrase_cfg = with_credentials_env(IEK_JWT, Some(IEK_PASSPHRASE), || {
            IntegrationsConfig::from_env().expect("passphrase INTEGRATIONS_ENCRYPTION_KEY")
        });
        assert_dedicated_key(&passphrase_cfg, IEK_PASSPHRASE, IEK_JWT, "passphrase");

        // Dedicated 64 hex → raw 32 bytes; not the JWT key.
        let hex_cfg = with_credentials_env(IEK_JWT, Some(IEK_HEX64), || {
            IntegrationsConfig::from_env().expect("64-hex INTEGRATIONS_ENCRYPTION_KEY")
        });
        assert_dedicated_key(&hex_cfg, IEK_HEX64, IEK_JWT, "64-hex");
    }

    #[test]
    fn integrations_encryption_key_unset_falls_back_to_jwt() {
        assert!(IEK_JWT.len() >= 32);
        let jwt_key = CredentialsKey::from_secret(IEK_JWT).expect("jwt key");
        let unset_cfg = with_credentials_env(IEK_JWT, None, || {
            IntegrationsConfig::from_env()
                .expect("unset INTEGRATIONS_ENCRYPTION_KEY must fall back to JWT_SECRET")
        });
        assert!(
            credentials_keys_match(&unset_cfg.credentials_key, &jwt_key),
            "unset: credentials_key must be derived from JWT_SECRET"
        );
        assert_eq!(unset_cfg.state_secret, IEK_JWT);
    }

    #[test]
    fn integrations_encryption_key_empty_or_whitespace_falls_back_to_jwt() {
        assert!(IEK_JWT.len() >= 32);
        let jwt_key = CredentialsKey::from_secret(IEK_JWT).expect("jwt key");
        for (case, value) in [("empty", ""), ("spaces", "   "), ("tabs_newlines", "\t\n")] {
            let cfg = with_credentials_env(IEK_JWT, Some(value), || {
                IntegrationsConfig::from_env().unwrap_or_else(|e| {
                    panic!(
                        "{case}: empty/whitespace INTEGRATIONS_ENCRYPTION_KEY must fall back \
                         to JWT_SECRET: {e:#}"
                    )
                })
            });
            assert!(
                credentials_keys_match(&cfg.credentials_key, &jwt_key),
                "{case}: credentials_key must be derived from JWT_SECRET"
            );
        }
    }

    #[test]
    fn integrations_encryption_key_64_hex_mixed_case_same_key() {
        assert!(IEK_JWT.len() >= 32);
        assert_eq!(IEK_HEX64.len(), 64);
        assert!(IEK_HEX64.chars().all(|c| c.is_ascii_hexdigit()));
        let upper = IEK_HEX64.to_ascii_uppercase();
        assert_ne!(IEK_HEX64, upper.as_str());

        let lower_cfg = with_credentials_env(IEK_JWT, Some(IEK_HEX64), || {
            IntegrationsConfig::from_env().expect("lowercase 64-hex INTEGRATIONS_ENCRYPTION_KEY")
        });
        let upper_cfg = with_credentials_env(IEK_JWT, Some(&upper), || {
            IntegrationsConfig::from_env().expect("uppercase 64-hex INTEGRATIONS_ENCRYPTION_KEY")
        });
        // Raw hex decode is case-insensitive; SHA-256 of aa… vs AA… differs.
        assert!(
            credentials_keys_match(&lower_cfg.credentials_key, &upper_cfg.credentials_key),
            "64-hex INTEGRATIONS_ENCRYPTION_KEY must be case-insensitive"
        );
        let jwt_key = CredentialsKey::from_secret(IEK_JWT).expect("jwt key");
        assert!(
            !credentials_keys_match(&lower_cfg.credentials_key, &jwt_key),
            "mixed-case hex must not fall back to JWT_SECRET"
        );
    }

    #[test]
    fn integrations_encryption_key_padded_64_hex_uses_dedicated() {
        assert!(IEK_JWT.len() >= 32);
        let padded = format!("  {IEK_HEX64}  ");
        assert_ne!(padded.as_str(), IEK_HEX64);
        assert_eq!(padded.trim(), IEK_HEX64);
        let cfg = with_credentials_env(IEK_JWT, Some(&padded), || {
            IntegrationsConfig::from_env().expect("padded 64-hex INTEGRATIONS_ENCRYPTION_KEY")
        });
        // Compare to unpadded hex so from_secret trim is required, not SHA-256 of spaces.
        assert_dedicated_key(&cfg, IEK_HEX64, IEK_JWT, "padded_64_hex");
    }

    /// Distinct from list-jwt-secret-unset-fails-closed (JWT is set and valid).
    const PAIR_JWT: &str = "oauth-client-pair-jwt-at-least-32-chars!";
    const PAIR_GH_ID: &str = "gh-oauth-pair-id-unique-aaaa";
    const PAIR_GH_SECRET: &str = "gh-oauth-pair-secret-unique-bbbb";
    const PAIR_GL_ID: &str = "gl-oauth-pair-id-unique-cccc";
    const PAIR_GL_SECRET: &str = "gl-oauth-pair-secret-unique-dddd";
    const PAIR_SLACK_ID: &str = "sl-oauth-pair-id-unique-eeee";
    const PAIR_SLACK_SECRET: &str = "sl-oauth-pair-secret-unique-ffff";

    /// JWT_SECRET + GitHub/GitLab OAuth pair under the same env_lock; all restored.
    fn with_oauth_client_pair_env<T>(
        github_id: Option<&str>,
        github_secret: Option<&str>,
        gitlab_id: Option<&str>,
        gitlab_secret: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let keys = [
            "JWT_SECRET",
            "GITHUB_CLIENT_ID",
            "GITHUB_CLIENT_SECRET",
            "GITLAB_CLIENT_ID",
            "GITLAB_CLIENT_SECRET",
        ];
        let previous: [Option<String>; 5] = std::array::from_fn(|i| env::var(keys[i]).ok());
        let next = [
            Some(PAIR_JWT),
            github_id,
            github_secret,
            gitlab_id,
            gitlab_secret,
        ];
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            for (k, v) in keys.iter().zip(next) {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            for (k, prev) in keys.iter().zip(previous) {
                match prev {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// JWT_SECRET + Slack pair; GitHub/GitLab unset. All restored.
    fn with_oauth_slack_pair_env<T>(
        slack_id: Option<&str>,
        slack_secret: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let keys = [
            "JWT_SECRET",
            "GITHUB_CLIENT_ID",
            "GITHUB_CLIENT_SECRET",
            "GITLAB_CLIENT_ID",
            "GITLAB_CLIENT_SECRET",
            "SLACK_CLIENT_ID",
            "SLACK_CLIENT_SECRET",
        ];
        let previous: [Option<String>; 7] = std::array::from_fn(|i| env::var(keys[i]).ok());
        let next = [
            Some(PAIR_JWT),
            None,
            None,
            None,
            None,
            slack_id,
            slack_secret,
        ];
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            for (k, v) in keys.iter().zip(next) {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            for (k, prev) in keys.iter().zip(previous) {
                match prev {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn assert_oauth_client_pair_rejected(
        result: anyhow::Result<IntegrationsConfig>,
        case: &str,
        required_key: &str,
        provider: &str,
    ) {
        match result {
            Ok(cfg) => {
                let half = match provider {
                    "github" => cfg.github.as_ref(),
                    "gitlab" => cfg.gitlab.as_ref(),
                    other => panic!("{case}: test bug: unknown provider {other}"),
                };
                panic!(
                    "{case}: from_env must fail when the OAuth pair is half-set; \
                     must not return {provider}: Some with an empty secret \
                     (configured={}, empty_secret={})",
                    half.is_some(),
                    half.is_some_and(|c| c.client_secret.is_empty()),
                );
            }
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains(required_key),
                    "{case}: error must mention {required_key}, got {msg}"
                );
                assert!(
                    !msg.contains(PAIR_JWT),
                    "{case}: error must not print JWT_SECRET"
                );
                assert!(
                    !msg.contains(PAIR_GH_SECRET),
                    "{case}: error must not print the OAuth secret"
                );
            }
        }
    }

    #[test]
    fn oauth_client_pair_half_set_fails_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        let missing = [None, Some(""), Some("   "), Some("\t\n")];

        for (i, secret) in missing.iter().copied().enumerate() {
            assert_oauth_client_pair_rejected(
                with_oauth_client_pair_env(
                    Some(PAIR_GH_ID),
                    secret,
                    None,
                    None,
                    IntegrationsConfig::from_env,
                ),
                &format!("github_id_without_secret_{i}"),
                "GITHUB_CLIENT_SECRET",
                "github",
            );
        }

        for (i, id) in missing.iter().copied().enumerate() {
            assert_oauth_client_pair_rejected(
                with_oauth_client_pair_env(
                    id,
                    Some(PAIR_GH_SECRET),
                    None,
                    None,
                    IntegrationsConfig::from_env,
                ),
                &format!("github_secret_without_id_{i}"),
                "GITHUB_CLIENT_ID",
                "github",
            );
        }

        // Not GitHub-only: one other provider half-set is enough.
        assert_oauth_client_pair_rejected(
            with_oauth_client_pair_env(
                None,
                None,
                Some(PAIR_GL_ID),
                None,
                IntegrationsConfig::from_env,
            ),
            "gitlab_id_without_secret",
            "GITLAB_CLIENT_SECRET",
            "gitlab",
        );
    }

    #[test]
    fn oauth_client_pair_unset_or_whitespace_boots_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        // Both unset / whitespace still boots — Connect starts unconfigured.
        for (case, id, secret) in [
            ("unset", None, None),
            ("empty", Some(""), Some("")),
            ("spaces", Some("   "), Some("   ")),
            ("tabs_newlines", Some("\t\n"), Some("\t\n")),
        ] {
            let cfg = with_oauth_client_pair_env(id, secret, None, None, || {
                IntegrationsConfig::from_env().unwrap_or_else(|e| {
                    panic!(
                        "{case}: both unset/whitespace must still Ok \
                         with github: None: {e:#}"
                    )
                })
            });
            assert!(
                cfg.github.is_none(),
                "{case}: github must be None, not a half-config"
            );
            assert!(cfg.gitlab.is_none(), "{case}: gitlab must stay None");
            assert_eq!(cfg.state_secret, PAIR_JWT);
        }
    }

    #[test]
    fn oauth_client_pair_both_set_parses_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        let cfg =
            with_oauth_client_pair_env(Some(PAIR_GH_ID), Some(PAIR_GH_SECRET), None, None, || {
                IntegrationsConfig::from_env().expect("both set must parse github")
            });
        let github = cfg
            .github
            .as_ref()
            .expect("both set must return github: Some");
        assert_eq!(github.client_id, PAIR_GH_ID);
        assert_eq!(github.client_secret, PAIR_GH_SECRET);
        assert!(
            github.github_app_slug.is_none(),
            "optional_oauth stores github_app_slug: None"
        );
        assert!(cfg.gitlab.is_none(), "gitlab must stay None when unset");
        assert_eq!(cfg.state_secret, PAIR_JWT);
    }

    #[test]
    fn oauth_client_id_and_secret_are_trimmed_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        let padded_id = format!("  {PAIR_GH_ID}  ");
        let padded_secret = format!("  {PAIR_GH_SECRET}  ");
        assert_ne!(padded_id.as_str(), PAIR_GH_ID);
        assert_ne!(padded_secret.as_str(), PAIR_GH_SECRET);
        let cfg = with_oauth_client_pair_env(
            Some(padded_id.as_str()),
            Some(padded_secret.as_str()),
            None,
            None,
            || IntegrationsConfig::from_env().expect("padded github pair must trim"),
        );
        let github = cfg
            .github
            .as_ref()
            .expect("padded pair must return github: Some");
        assert_eq!(github.client_id, PAIR_GH_ID);
        assert_eq!(github.client_secret, PAIR_GH_SECRET);
    }

    #[test]
    fn oauth_client_pair_gitlab_both_set_parses_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        let cfg =
            with_oauth_client_pair_env(None, None, Some(PAIR_GL_ID), Some(PAIR_GL_SECRET), || {
                IntegrationsConfig::from_env().expect("both set must parse gitlab")
            });
        let gitlab = cfg
            .gitlab
            .as_ref()
            .expect("both set must return gitlab: Some");
        assert_eq!(gitlab.client_id, PAIR_GL_ID);
        assert_eq!(gitlab.client_secret, PAIR_GL_SECRET);
        assert!(cfg.github.is_none(), "github must stay None when unset");
        assert_eq!(cfg.state_secret, PAIR_JWT);
    }

    #[test]
    fn oauth_client_pair_slack_both_set_parses_from_env() {
        assert!(PAIR_JWT.len() >= 32);
        let cfg = with_oauth_slack_pair_env(Some(PAIR_SLACK_ID), Some(PAIR_SLACK_SECRET), || {
            IntegrationsConfig::from_env().expect("both set must parse slack")
        });
        let slack = cfg
            .slack
            .as_ref()
            .expect("both set must return slack: Some");
        assert_eq!(slack.client_id, PAIR_SLACK_ID);
        assert_eq!(slack.client_secret, PAIR_SLACK_SECRET);
        assert!(cfg.github.is_none(), "github must stay None when unset");
        assert!(cfg.gitlab.is_none(), "gitlab must stay None when unset");
        assert_eq!(cfg.state_secret, PAIR_JWT);
    }
}
