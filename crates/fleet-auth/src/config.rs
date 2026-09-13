use std::env;
use std::time::Duration;

use anyhow::bail;

/// Auth-specific settings (JWT + cookies). Server bind/CORS live in `fleet-server`.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    pub cookie_secure: bool,
    pub cookie_samesite: cookie::SameSite,
    /// When set, POST /api/auth/signup requires this shared token.
    pub signup_token: Option<String>,
    /// Normalized emails that are always granted `users.instance_admin` on auth.
    pub instance_admin_emails: Vec<String>,
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let jwt_secret = env::var("JWT_SECRET")
            .unwrap_or_else(|_| "dev-change-me-to-a-long-random-secret-at-least-32-chars".into());
        if jwt_secret.len() < 32 {
            bail!("JWT_SECRET must be at least 32 characters");
        }

        let access_ttl = Duration::from_secs(
            env::var("JWT_ACCESS_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(900),
        );
        let refresh_ttl = Duration::from_secs(
            env::var("JWT_REFRESH_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(604_800),
        );

        let cookie_secure = env::var("COOKIE_SECURE")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let cookie_samesite = match env::var("COOKIE_SAMESITE")
            .unwrap_or_else(|_| "Lax".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "strict" => cookie::SameSite::Strict,
            "none" => cookie::SameSite::None,
            _ => cookie::SameSite::Lax,
        };

        let signup_token = env::var("SIGNUP_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(Self {
            jwt_secret,
            access_ttl,
            refresh_ttl,
            cookie_secure,
            cookie_samesite,
            signup_token,
            instance_admin_emails: instance_admin_emails_from_env(),
        })
    }

    pub fn email_is_env_instance_admin(&self, email_normalized: &str) -> bool {
        self.instance_admin_emails
            .iter()
            .any(|e| e == email_normalized)
    }
}

/// A public (non-loopback) API / worker / UI base requires `SIGNUP_TOKEN`.
/// Local `fleet-dev` (`localhost`, `127/8`, `::1`) may keep open signup.
pub fn public_url_requires_signup_token(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    !agent_fleet_contracts::url_is_loopback(url)
}

/// Fail closed when any public base is reachable from the internet and
/// signup is unauthenticated. Empty token + loopback URLs is local dev.
pub fn validate_signup_policy(
    signup_token: Option<&str>,
    public_bases: &[&str],
) -> Result<(), String> {
    if signup_token.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return Ok(());
    }
    for url in public_bases {
        if public_url_requires_signup_token(url) {
            return Err(format!(
                "SIGNUP_TOKEN is required when `{url}` is not loopback \
                 (open signup on a public URL)"
            ));
        }
    }
    Ok(())
}

/// Comma-separated emails. Trimmed, lowercased, must contain `@`.
pub fn instance_admin_emails_from_env() -> Vec<String> {
    parse_instance_admin_emails(env::var("INSTANCE_ADMIN_EMAILS").ok().as_deref())
}

pub fn parse_instance_admin_emails(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && s.contains('@'))
        .collect()
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

    #[test]
    fn jwt_secret_shorter_than_32_fails_from_env() {
        let err = with_jwt_secret(Some("too-short"), || {
            AuthConfig::from_env().expect_err("short JWT_SECRET must fail")
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
            AuthConfig::from_env().expect("valid JWT_SECRET")
        });
        assert_eq!(cfg.jwt_secret, secret);
    }

    #[test]
    fn from_env_reads_signup_token() {
        let _guard = env_lock().lock().expect("env lock");
        let previous = env::var("SIGNUP_TOKEN").ok();
        unsafe { env::set_var("SIGNUP_TOKEN", "  invite-abc  ") };
        let cfg = AuthConfig::from_env().expect("from_env");
        unsafe {
            match previous {
                Some(v) => env::set_var("SIGNUP_TOKEN", v),
                None => env::remove_var("SIGNUP_TOKEN"),
            }
        }
        assert_eq!(cfg.signup_token.as_deref(), Some("invite-abc"));
    }

    #[test]
    fn public_url_requires_signup_token_only_for_non_loopback() {
        assert!(!public_url_requires_signup_token(""));
        assert!(!public_url_requires_signup_token("   "));
        assert!(!public_url_requires_signup_token("http://127.0.0.1:8080"));
        assert!(!public_url_requires_signup_token("http://localhost:3030"));
        assert!(!public_url_requires_signup_token("http://[::1]:8080"));
        assert!(public_url_requires_signup_token("https://fleet.example.test"));
        assert!(public_url_requires_signup_token("http://10.0.0.8:8080"));
    }

    #[test]
    fn validate_signup_policy_allows_loopback_without_token() {
        assert!(validate_signup_policy(
            None,
            &["http://127.0.0.1:8080", "http://localhost:3030"]
        )
        .is_ok());
    }

    #[test]
    fn validate_signup_policy_requires_token_on_public_base() {
        let err = validate_signup_policy(None, &["https://api.example.test"])
            .expect_err("public URL without token");
        assert!(
            err.contains("SIGNUP_TOKEN") && err.contains("api.example.test"),
            "{err}"
        );
        assert!(validate_signup_policy(
            Some("invite-xyz"),
            &["https://api.example.test"]
        )
        .is_ok());
    }

    #[test]
    fn parse_instance_admin_emails_normalizes() {
        assert_eq!(
            parse_instance_admin_emails(Some("  Ada@Example.TEST ,bob@x.test,,")),
            vec!["ada@example.test".to_string(), "bob@x.test".to_string()]
        );
        assert!(parse_instance_admin_emails(Some("")).is_empty());
        assert!(parse_instance_admin_emails(Some("not-an-email")).is_empty());
    }
}
