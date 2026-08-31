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
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let jwt_secret = match env::var("JWT_SECRET") {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    bail!("JWT_SECRET is required and must not be empty or whitespace-only");
                }
                trimmed.to_string()
            }
            _ => bail!("JWT_SECRET is required and must not be empty or whitespace-only"),
        };
        if jwt_secret.len() < 32 {
            bail!("JWT_SECRET must be at least 32 characters");
        }

        // 0 is Max-Age=0 (delete-cookie), not "disable cookies". Unset still
        // defaults to the documented 900 / 604800.
        let access_ttl = ttl_secs_from_env("JWT_ACCESS_TTL_SECS", 900)?;
        let refresh_ttl = ttl_secs_from_env("JWT_REFRESH_TTL_SECS", 604_800)?;
        if refresh_ttl < access_ttl {
            bail!(
                "JWT_REFRESH_TTL_SECS ({refresh_ttl:?}) must be >= JWT_ACCESS_TTL_SECS \
                 ({access_ttl:?})"
            );
        }

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

        let cfg = Self {
            jwt_secret,
            access_ttl,
            refresh_ttl,
            cookie_secure,
            cookie_samesite,
        };
        cfg.validate_cookie_policy()?;
        Ok(cfg)
    }

    /// Fail closed: SameSite=None requires Secure. In-struct configs may
    /// still coerce at mint time via `base_cookie`.
    pub fn validate_cookie_policy(&self) -> anyhow::Result<()> {
        if matches!(self.cookie_samesite, cookie::SameSite::None) && !self.cookie_secure {
            bail!(
                "COOKIE_SAMESITE=None requires COOKIE_SECURE=true \
                 (browsers reject SameSite=None without Secure)"
            );
        }
        Ok(())
    }
}

/// Parse a JWT TTL env var. Unset or unparseable uses `default_secs`.
/// Explicit `0` fails closed — Set-Cookie Max-Age=0 deletes the session.
fn ttl_secs_from_env(name: &str, default_secs: u64) -> anyhow::Result<Duration> {
    let secs = match env::var(name) {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(0) => bail!(
                "{name} must be a positive number of seconds \
                 (0 sets Max-Age=0 and deletes the session cookie)"
            ),
            Ok(secs) => secs,
            Err(_) => default_secs,
        },
        Err(_) => default_secs,
    };
    Ok(Duration::from_secs(secs))
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

    fn assert_jwt_secret_rejected(result: anyhow::Result<AuthConfig>, case: &str) {
        match result {
            Ok(cfg) => panic!(
                "{case}: JWT_SECRET must fail from_env; must not boot with the public default \
                 (got jwt_secret==public_default={}, len={})",
                cfg.jwt_secret == PUBLIC_DEFAULT,
                cfg.jwt_secret.len()
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
        // Unset must not unwrap_or the published default (anyone can forge cookies).
        assert_jwt_secret_rejected(with_jwt_secret(None, AuthConfig::from_env), "unset");
        for (case, value) in [
            ("empty", ""),
            ("spaces", "   "),
            ("tabs_newlines", "\t\n"),
            // 32 spaces pass the length gate today; whitespace-only is still not a secret.
            ("thirty_two_spaces", "                                "),
        ] {
            assert_jwt_secret_rejected(with_jwt_secret(Some(value), AuthConfig::from_env), case);
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
    fn jwt_secret_padded_short_fails_from_env() {
        // Untrimmed length is >= 32; trimmed is well under.
        let padded_short = format!("too-short{}", " ".repeat(32));
        assert!(padded_short.len() >= 32);
        assert!(padded_short.trim().len() < 32);
        let err = with_jwt_secret(Some(&padded_short), || {
            AuthConfig::from_env().expect_err("padded short JWT_SECRET must fail")
        });
        let msg = format!("{err:#}");
        assert!(
            msg.contains("JWT_SECRET") && msg.contains("32"),
            "unexpected error: {msg}"
        );

        let leading = format!("{}too-short", " ".repeat(32));
        assert!(leading.len() >= 32);
        assert!(leading.trim().len() < 32);
        assert_jwt_secret_rejected(
            with_jwt_secret(Some(&leading), AuthConfig::from_env),
            "leading_pad_short",
        );
    }

    #[test]
    fn valid_secret_from_env_works() {
        let secret = "valid-secret-at-least-32-characters-long!";
        let cfg = with_jwt_secret(Some(secret), || {
            AuthConfig::from_env().expect("valid JWT_SECRET")
        });
        assert_eq!(cfg.jwt_secret, secret);
        assert_ne!(cfg.jwt_secret, PUBLIC_DEFAULT);
    }

    #[test]
    fn jwt_secret_surrounding_whitespace_is_trimmed() {
        let secret = "valid-secret-at-least-32-characters-long!";
        let padded = format!("  {secret}\t\n");
        let cfg = with_jwt_secret(Some(&padded), || {
            AuthConfig::from_env().expect("trimmed JWT_SECRET must parse")
        });
        assert_eq!(cfg.jwt_secret, secret);
        assert_ne!(cfg.jwt_secret, padded);
    }

    #[test]
    fn auth_config_constructs_in_struct() {
        let cfg = AuthConfig {
            jwt_secret: "in-struct-secret-at-least-32-chars!!!!".into(),
            access_ttl: Duration::from_secs(60),
            refresh_ttl: Duration::from_secs(3600),
            cookie_secure: false,
            cookie_samesite: cookie::SameSite::Lax,
        };
        assert!(cfg.jwt_secret.len() >= 32);
        assert!(!cfg.cookie_secure);
    }

    /// JWT_SECRET + TTL env under the same lock; cookie policy pinned to Lax
    /// so a foreign SameSite=None does not fail this case for the wrong reason.
    fn with_ttl_env<T>(
        access_ttl: Option<&str>,
        refresh_ttl: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let prev_jwt = env::var("JWT_SECRET").ok();
        let prev_access = env::var("JWT_ACCESS_TTL_SECS").ok();
        let prev_refresh = env::var("JWT_REFRESH_TTL_SECS").ok();
        let prev_ss = env::var("COOKIE_SAMESITE").ok();
        let prev_sec = env::var("COOKIE_SECURE").ok();
        const VALID: &str = "valid-secret-at-least-32-characters-long!";
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            env::set_var("JWT_SECRET", VALID);
            match access_ttl {
                Some(v) => env::set_var("JWT_ACCESS_TTL_SECS", v),
                None => env::remove_var("JWT_ACCESS_TTL_SECS"),
            }
            match refresh_ttl {
                Some(v) => env::set_var("JWT_REFRESH_TTL_SECS", v),
                None => env::remove_var("JWT_REFRESH_TTL_SECS"),
            }
            env::set_var("COOKIE_SAMESITE", "Lax");
            env::set_var("COOKIE_SECURE", "false");
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            match prev_jwt {
                Some(v) => env::set_var("JWT_SECRET", v),
                None => env::remove_var("JWT_SECRET"),
            }
            match prev_access {
                Some(v) => env::set_var("JWT_ACCESS_TTL_SECS", v),
                None => env::remove_var("JWT_ACCESS_TTL_SECS"),
            }
            match prev_refresh {
                Some(v) => env::set_var("JWT_REFRESH_TTL_SECS", v),
                None => env::remove_var("JWT_REFRESH_TTL_SECS"),
            }
            match prev_ss {
                Some(v) => env::set_var("COOKIE_SAMESITE", v),
                None => env::remove_var("COOKIE_SAMESITE"),
            }
            match prev_sec {
                Some(v) => env::set_var("COOKIE_SECURE", v),
                None => env::remove_var("COOKIE_SECURE"),
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// 0 is not "disable cookies": Max-Age=0 deletes the session. Fail closed
    /// or clamp (access >=60s or documented 900; refresh >= access).
    fn assert_jwt_ttl_not_zero(result: anyhow::Result<AuthConfig>, env_name: &str, case: &str) {
        match result {
            Ok(cfg) => {
                assert_ne!(
                    cfg.access_ttl,
                    Duration::ZERO,
                    "{case}: access_ttl must not be Duration::ZERO \
                     (Set-Cookie Max-Age=0 deletes fleet_access)"
                );
                assert_ne!(
                    cfg.refresh_ttl,
                    Duration::ZERO,
                    "{case}: refresh_ttl must not be Duration::ZERO \
                     (Set-Cookie Max-Age=0 deletes fleet_refresh)"
                );
                assert!(
                    cfg.access_ttl >= Duration::from_secs(60),
                    "{case}: access_ttl must be >=60s (or documented 900), got {:?}",
                    cfg.access_ttl
                );
                assert!(
                    cfg.refresh_ttl >= cfg.access_ttl,
                    "{case}: refresh_ttl {:?} must be >= access_ttl {:?}",
                    cfg.refresh_ttl,
                    cfg.access_ttl
                );
            }
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains(env_name),
                    "{case}: error must mention {env_name}, got {msg}"
                );
            }
        }
    }

    #[test]
    fn jwt_ttl_zero_fails_from_env() {
        // Each env independently; the other stays at the documented default.
        assert_jwt_ttl_not_zero(
            with_ttl_env(Some("0"), None, AuthConfig::from_env),
            "JWT_ACCESS_TTL_SECS",
            "JWT_ACCESS_TTL_SECS=0",
        );
        assert_jwt_ttl_not_zero(
            with_ttl_env(None, Some("0"), AuthConfig::from_env),
            "JWT_REFRESH_TTL_SECS",
            "JWT_REFRESH_TTL_SECS=0",
        );
    }

    #[test]
    fn jwt_ttl_unset_and_explicit_defaults_parse() {
        let unset = with_ttl_env(None, None, || {
            AuthConfig::from_env().expect("unset JWT_*_TTL_SECS must default")
        });
        assert_eq!(unset.access_ttl, Duration::from_secs(900));
        assert_eq!(unset.refresh_ttl, Duration::from_secs(604_800));

        let explicit = with_ttl_env(Some("900"), Some("604800"), || {
            AuthConfig::from_env().expect("explicit 900 / 604800 must parse")
        });
        assert_eq!(explicit.access_ttl, Duration::from_secs(900));
        assert_eq!(explicit.refresh_ttl, Duration::from_secs(604_800));
    }

    fn with_cookie_env<T>(
        cookie_samesite: Option<&str>,
        cookie_secure: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let prev_jwt = env::var("JWT_SECRET").ok();
        let prev_ss = env::var("COOKIE_SAMESITE").ok();
        let prev_sec = env::var("COOKIE_SECURE").ok();
        const VALID: &str = "valid-secret-at-least-32-characters-long!";
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            env::set_var("JWT_SECRET", VALID);
            match cookie_samesite {
                Some(v) => env::set_var("COOKIE_SAMESITE", v),
                None => env::remove_var("COOKIE_SAMESITE"),
            }
            match cookie_secure {
                Some(v) => env::set_var("COOKIE_SECURE", v),
                None => env::remove_var("COOKIE_SECURE"),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            match prev_jwt {
                Some(v) => env::set_var("JWT_SECRET", v),
                None => env::remove_var("JWT_SECRET"),
            }
            match prev_ss {
                Some(v) => env::set_var("COOKIE_SAMESITE", v),
                None => env::remove_var("COOKIE_SAMESITE"),
            }
            match prev_sec {
                Some(v) => env::set_var("COOKIE_SECURE", v),
                None => env::remove_var("COOKIE_SECURE"),
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn assert_samesite_none_without_secure_rejected(
        result: anyhow::Result<AuthConfig>,
        case: &str,
    ) {
        match result {
            Ok(cfg) => {
                assert!(
                    !(matches!(cfg.cookie_samesite, cookie::SameSite::None) && !cfg.cookie_secure),
                    "{case}: from_env must fail or coerce; must not accept \
                     SameSite=None without Secure (got samesite={:?} secure={})",
                    cfg.cookie_samesite,
                    cfg.cookie_secure
                );
            }
            Err(err) => {
                let msg = format!("{err:#}");
                let lower = msg.to_ascii_lowercase();
                assert!(
                    lower.contains("samesite") && lower.contains("secure"),
                    "{case}: error must mention SameSite and Secure, got {msg}"
                );
            }
        }
    }

    #[test]
    fn samesite_none_without_secure_fails_from_env() {
        // Unset / false / 0 must not mint a None+insecure cookie policy.
        for (case, secure) in [("unset", None), ("false", Some("false")), ("0", Some("0"))] {
            assert_samesite_none_without_secure_rejected(
                with_cookie_env(Some("none"), secure, AuthConfig::from_env),
                case,
            );
        }
    }

    #[test]
    fn samesite_none_with_secure_parses_from_env() {
        let cfg = with_cookie_env(Some("none"), Some("true"), || {
            AuthConfig::from_env().expect("COOKIE_SAMESITE=none + COOKIE_SECURE=true must parse")
        });
        assert!(
            matches!(cfg.cookie_samesite, cookie::SameSite::None),
            "expected SameSite::None, got {:?}",
            cfg.cookie_samesite
        );
        assert!(cfg.cookie_secure);
    }

    #[test]
    fn samesite_lax_without_secure_from_env_ok() {
        let default = with_cookie_env(None, None, || {
            AuthConfig::from_env().expect("default Lax + insecure must parse")
        });
        assert!(matches!(default.cookie_samesite, cookie::SameSite::Lax));
        assert!(!default.cookie_secure);

        let explicit = with_cookie_env(Some("Lax"), Some("false"), || {
            AuthConfig::from_env().expect("Lax + COOKIE_SECURE=false must parse")
        });
        assert!(matches!(explicit.cookie_samesite, cookie::SameSite::Lax));
        assert!(!explicit.cookie_secure);
    }
}
