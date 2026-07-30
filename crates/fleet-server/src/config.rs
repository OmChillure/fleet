use std::env;
use std::net::SocketAddr;

use anyhow::Context;
use fleet_auth::AuthConfig;

/// Full server configuration (bind + CORS + auth + database).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub database_url: String,
    pub bind: SocketAddr,
    pub cors_origins: Vec<String>,
    pub auth: AuthConfig,
}

impl ServerConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env::var("DATABASE_URL")
            .context("DATABASE_URL is required (Neon, Supabase, or local Postgres)")?;

        // Prefer HOST/PORT; fall back to AUTH_HOST/AUTH_PORT for older .env files.
        // Backend default: 127.0.0.1:8080 (UI on localhost:3030). Use an IP for bind
        // — `localhost` is not a valid SocketAddr host.
        let host = env::var("HOST")
            .or_else(|_| env::var("AUTH_HOST"))
            .unwrap_or_else(|_| "127.0.0.1".into());
        let port: u16 = env::var("PORT")
            .or_else(|_| env::var("AUTH_PORT"))
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8080);

        let bind: SocketAddr = format!("{host}:{port}")
            .parse()
            .with_context(|| format!("invalid HOST/PORT: {host}:{port}"))?;

        let cors_origins = env::var("CORS_ORIGINS")
            .unwrap_or_else(|_| "http://localhost:3030,http://127.0.0.1:3030".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let auth = AuthConfig::from_env()?;

        Ok(Self {
            database_url,
            bind,
            cors_origins,
            auth,
        })
    }
}
