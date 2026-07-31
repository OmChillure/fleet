//! Process configuration from environment (design §Config Knobs).

use std::env;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Token,
    None,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::None => "none",
        }
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub admin_token: String,
    pub auth_mode: AuthMode,
    pub http_addr: String,
    pub data_dir: PathBuf,
    pub tick_interval: Duration,
    pub max_agents: i32,
    pub heartbeat_stale: Duration,
    pub spawn_timeout: Duration,
    pub kill_grace: Duration,
    pub default_task_timeout: Duration,
    pub default_max_retries: i32,
    pub retry_backoff: Duration,
    pub on_task_fail: String,
    pub auto_sync_external: bool,
    pub sandbox_backend: String,
    pub stub_duration: Duration,
    pub lock_wait_timeout: Duration,
    pub log_level: String,
    pub leadership_mode: String,
    pub project_filter: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Message(String),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let auth_mode = match env_or("AUTH_MODE", "token").as_str() {
            "token" => AuthMode::Token,
            "none" => AuthMode::None,
            other => {
                return Err(ConfigError::Message(format!("invalid AUTH_MODE {other:?}")));
            }
        };

        let database_url = env::var("DATABASE_URL").unwrap_or_default();
        if database_url.trim().is_empty() {
            return Err(ConfigError::Message("DATABASE_URL is required".into()));
        }

        let admin_token = env::var("ADMIN_TOKEN").unwrap_or_default();
        let http_addr = env_or("HTTP_ADDR", ":8080");

        match auth_mode {
            AuthMode::Token if admin_token.trim().is_empty() => {
                return Err(ConfigError::Message(
                    "ADMIN_TOKEN is required when AUTH_MODE=token".into(),
                ));
            }
            AuthMode::None if !is_loopback_addr(&http_addr) => {
                return Err(ConfigError::Message(format!(
                    "AUTH_MODE=none requires loopback HTTP_ADDR, got {http_addr:?}"
                )));
            }
            _ => {}
        }

        let data_dir_raw = env_or("DATA_DIR", "./data");
        let data_dir = std::fs::canonicalize(&data_dir_raw)
            .or_else(|_| {
                // Create if missing for abs path resolution of relative paths
                let p = PathBuf::from(&data_dir_raw);
                if !p.exists() {
                    let _ = std::fs::create_dir_all(&p);
                }
                std::fs::canonicalize(&p).or_else(|_| {
                    // fallback: join cwd
                    env::current_dir().map(|cwd| cwd.join(&data_dir_raw))
                })
            })
            .map_err(|e| ConfigError::Message(format!("DATA_DIR: {e}")))?;

        let tick_interval = duration_secs("TICK_INTERVAL_S", 5);
        let lock_wait = env::var("LOCK_WAIT_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(tick_interval);

        Ok(Self {
            database_url,
            admin_token,
            auth_mode,
            http_addr,
            data_dir,
            tick_interval,
            max_agents: int_env("MAX_AGENTS", 3),
            heartbeat_stale: duration_secs("HEARTBEAT_STALE_S", 60),
            spawn_timeout: duration_secs("SPAWN_TIMEOUT_S", 30),
            kill_grace: duration_secs("KILL_GRACE_S", 15),
            default_task_timeout: duration_secs("DEFAULT_TASK_TIMEOUT_S", 3600),
            default_max_retries: int_env("DEFAULT_MAX_RETRIES", 2),
            retry_backoff: duration_secs("RETRY_BACKOFF_S", 10),
            on_task_fail: env_or("ON_TASK_FAIL", "fail_run"),
            auto_sync_external: bool_env("AUTO_SYNC_EXTERNAL", false),
            sandbox_backend: env_or("SANDBOX_BACKEND", "stub"),
            stub_duration: duration_secs("STUB_DURATION_S", 3),
            lock_wait_timeout: lock_wait,
            log_level: env_or("LOG_LEVEL", "info"),
            leadership_mode: "single_process".into(),
            project_filter: env::var("PROJECT_ID").ok().filter(|s| !s.trim().is_empty()),
        })
    }
}

fn env_or(k: &str, def: &str) -> String {
    env::var(k)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| def.to_string())
}

fn int_env(k: &str, def: i32) -> i32 {
    env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(def)
}

fn duration_secs(k: &str, def: u64) -> Duration {
    Duration::from_secs(
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(def),
    )
}

fn bool_env(k: &str, def: bool) -> bool {
    match env::var(k) {
        Ok(v) => matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => def,
    }
}

fn is_loopback_addr(addr: &str) -> bool {
    if addr.starts_with(':') {
        return false; // :8080 binds all interfaces
    }
    let host = addr
        .rsplit_once(':')
        .map(|(h, _)| h.trim_matches(|c| c == '[' || c == ']'))
        .unwrap_or(addr);
    host == "127.0.0.1" || host == "localhost" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env tests
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_relevant() {
        for k in [
            "DATABASE_URL",
            "ADMIN_TOKEN",
            "AUTH_MODE",
            "HTTP_ADDR",
            "DATA_DIR",
            "TICK_INTERVAL_S",
            "MAX_AGENTS",
            "SANDBOX_BACKEND",
            "LOCK_WAIT_TIMEOUT_S",
        ] {
            unsafe { env::remove_var(k) };
        }
    }

    #[test]
    fn load_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var(
            "DATABASE_URL",
            "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
        ) };
        unsafe { env::set_var("ADMIN_TOKEN", "test-admin-token") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.tick_interval, Duration::from_secs(5));
        assert_eq!(cfg.max_agents, 3);
        assert_eq!(cfg.auth_mode, AuthMode::Token);
        assert_eq!(cfg.sandbox_backend, "stub");
        assert_eq!(cfg.leadership_mode, "single_process");
        assert!(cfg.data_dir.is_absolute());
    }

    #[test]
    fn requires_database_url() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var("ADMIN_TOKEN", "x") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        assert!(Config::from_env().is_err());
    }

    #[test]
    fn requires_admin_token_in_token_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var("DATABASE_URL", "postgres://x") };
        unsafe { env::set_var("AUTH_MODE", "token") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        assert!(Config::from_env().is_err());
    }

    #[test]
    fn auth_none_requires_loopback() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var("DATABASE_URL", "postgres://x") };
        unsafe { env::set_var("AUTH_MODE", "none") };
        unsafe { env::set_var("HTTP_ADDR", ":8080") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        assert!(Config::from_env().is_err());

        unsafe { env::set_var("HTTP_ADDR", "127.0.0.1:8080") };
        assert!(Config::from_env().is_ok());
    }
}
