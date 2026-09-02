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
        })
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
    fn auth_config_constructs_in_struct() {
        let cfg = AuthConfig {
            jwt_secret: "in-struct-secret-at-least-32-chars!!!!".into(),
            access_ttl: Duration::from_secs(60),
            refresh_ttl: Duration::from_secs(3600),
            cookie_secure: false,
            cookie_samesite: cookie::SameSite::Lax,
            signup_token: None,
        };
        assert!(cfg.jwt_secret.len() >= 32);
        assert!(!cfg.cookie_secure);
        assert!(cfg.signup_token.is_none());
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
}
