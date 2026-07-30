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
        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| {
            "dev-change-me-to-a-long-random-secret-at-least-32-chars".into()
        });
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

        Ok(Self {
            jwt_secret,
            access_ttl,
            refresh_ttl,
            cookie_secure,
            cookie_samesite,
        })
    }
}
