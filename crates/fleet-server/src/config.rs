use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use fleet_auth::AuthConfig;
use fleet_integrations::IntegrationsConfig;

use crate::chat::OllamaConfig;
use crate::grok::GrokConfig;
use crate::providers::{ClaudeConfig, OpenaiConfig};

/// Full server configuration (bind + CORS + auth + database + integrations + LLM providers).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub database_url: String,
    pub bind: SocketAddr,
    pub cors_origins: Vec<String>,
    pub auth: AuthConfig,
    pub integrations: IntegrationsConfig,
    pub ollama: OllamaConfig,
    pub grok: GrokConfig,
    pub openai: OpenaiConfig,
    pub claude: ClaudeConfig,
    /// Optional prebuilt `fleet-host` / `fleet-driver` tree.
    /// First lookup for `/install/worker/bin/{platform}/{bin}`. When unset or
    /// the file is missing, the handler tries current_exe siblings and (native
    /// only) workspace `target/{release,debug}/{bin}`.
    pub worker_dist_dir: Option<PathBuf>,
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
        let integrations = IntegrationsConfig::from_env()?;
        let ollama = OllamaConfig::from_env();
        let grok = GrokConfig::from_env();
        let openai = OpenaiConfig::from_env();
        let claude = ClaudeConfig::from_env();
        let worker_dist_dir = env::var("FLEET_WORKER_DIST_DIR")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        Ok(Self {
            database_url,
            bind,
            cors_origins,
            auth,
            integrations,
            ollama,
            grok,
            openai,
            claude,
            worker_dist_dir,
        })
    }
}
