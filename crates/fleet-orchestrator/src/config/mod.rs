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
    /// Where spawned workers POST heartbeat/status. Snapshotted at Config
    /// build — do not re-read `API_PUBLIC_BASE` at spawn (Kind :8080 in `.env`
    /// would steal in-process test workers).
    pub orchestrator_public_url: String,
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
    /// Process backend worker argv (`PROCESS_WORKER_BIN`, whitespace-split).
    pub process_worker_argv: Vec<String>,
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
    /// Embed in fleet-server: reuse its DATABASE_URL and bind as public orch URL.
    pub fn from_env_for_server(database_url: &str, bind_addr: &str) -> Result<Self, ConfigError> {
        // Prefer explicit ORCHESTRATOR_DATABASE_URL when isolating schemas/DBs.
        let database_url = env::var("ORCHESTRATOR_DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| database_url.to_string());

        // Temporarily set DATABASE_URL for from_env path fields that read env.
        // Safer: build structurally.
        // Cookie/workspace fleet routes do not need ADMIN_TOKEN. Optional bearer
        // is only for /v1 automation when set.
        let admin_token = env::var("ADMIN_TOKEN").unwrap_or_default();
        let auth_mode = match env_or("ORCH_AUTH_MODE", "token").as_str() {
            "token" => AuthMode::Token,
            "none" => AuthMode::None,
            other => {
                return Err(ConfigError::Message(format!(
                    "invalid ORCH_AUTH_MODE {other:?}"
                )));
            }
        };
        let data_dir_raw = env_or("DATA_DIR", "./data");
        let data_dir = resolve_data_dir(&data_dir_raw)?;

        let tick_interval = duration_secs("TICK_INTERVAL_S", 5);
        Ok(Self {
            database_url,
            admin_token,
            auth_mode,
            http_addr: bind_addr.to_string(),
            orchestrator_public_url: public_orchestrator_url(bind_addr),
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
            process_worker_argv: process_worker_argv_from_env(),
            lock_wait_timeout: tick_interval,
            log_level: env_or("LOG_LEVEL", "info"),
            leadership_mode: "single_process".into(),
            project_filter: env::var("PROJECT_ID").ok().filter(|s| !s.trim().is_empty()),
        })
    }

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

        // ADMIN_TOKEN is optional: when unset, /v1 admin bearer routes reject;
        // cookie/workspace fleet routes do not use it.
        let admin_token = env::var("ADMIN_TOKEN").unwrap_or_default();
        let http_addr = env_or("HTTP_ADDR", ":8080");

        match auth_mode {
            AuthMode::None if !is_loopback_addr(&http_addr) => {
                return Err(ConfigError::Message(format!(
                    "AUTH_MODE=none requires loopback HTTP_ADDR, got {http_addr:?}"
                )));
            }
            _ => {}
        }

        let data_dir_raw = env_or("DATA_DIR", "./data");
        let data_dir = resolve_data_dir(&data_dir_raw)?;

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
            http_addr: http_addr.clone(),
            orchestrator_public_url: public_orchestrator_url(&http_addr),
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
            process_worker_argv: process_worker_argv_from_env(),
            lock_wait_timeout: lock_wait,
            log_level: env_or("LOG_LEVEL", "info"),
            leadership_mode: "single_process".into(),
            project_filter: env::var("PROJECT_ID").ok().filter(|s| !s.trim().is_empty()),
        })
    }
}

fn process_worker_argv_from_env() -> Vec<String> {
    match env::var("PROCESS_WORKER_BIN") {
        Ok(s) if !s.trim().is_empty() => s.split_whitespace().map(|p| p.to_string()).collect(),
        _ => vec!["fleet-worker".into()],
    }
}

/// URL workers use to reach the control plane (`FLEET_ORCHESTRATOR_URL`).
///
/// Prefers `ORCHESTRATOR_PUBLIC_URL` or `API_PUBLIC_BASE`. Wildcard binds
/// (`0.0.0.0`, `[::]`) are rewritten to loopback so spawned processes can connect.
pub fn public_orchestrator_url(http_addr: &str) -> String {
    for key in ["ORCHESTRATOR_PUBLIC_URL", "API_PUBLIC_BASE"] {
        if let Ok(v) = env::var(key) {
            let v = v.trim().trim_end_matches('/');
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    let raw = if http_addr.starts_with("http://") || http_addr.starts_with("https://") {
        http_addr.trim_end_matches('/').to_string()
    } else if http_addr.starts_with(':') {
        format!("http://127.0.0.1{}", http_addr)
    } else {
        format!("http://{}", http_addr.trim_end_matches('/'))
    };
    rewrite_wildcard_bind(&raw)
}

fn rewrite_wildcard_bind(url: &str) -> String {
    url.replace("://0.0.0.0:", "://127.0.0.1:")
        .replace("://[::]:", "://127.0.0.1:")
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

fn resolve_data_dir(data_dir_raw: &str) -> Result<PathBuf, ConfigError> {
    if let Ok(p) = std::fs::canonicalize(data_dir_raw) {
        return Ok(p);
    }
    let p = PathBuf::from(data_dir_raw);
    std::fs::create_dir_all(&p).map_err(|e| {
        ConfigError::Message(format!("DATA_DIR create {}: {e}", p.display()))
    })?;
    std::fs::canonicalize(&p).map_err(|e| ConfigError::Message(format!("DATA_DIR: {e}")))
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
            "API_PUBLIC_BASE",
            "ORCHESTRATOR_PUBLIC_URL",
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
    fn admin_token_optional_in_token_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var("DATABASE_URL", "postgres://x") };
        unsafe { env::set_var("AUTH_MODE", "token") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        let cfg = Config::from_env().expect("ADMIN_TOKEN is optional");
        assert!(cfg.admin_token.is_empty());
        assert_eq!(cfg.auth_mode, AuthMode::Token);
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

    #[test]
    fn public_url_rewrites_wildcard_and_prefers_env() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { env::remove_var("ORCHESTRATOR_PUBLIC_URL") };
        unsafe { env::remove_var("API_PUBLIC_BASE") };
        assert_eq!(
            public_orchestrator_url(":8080"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            public_orchestrator_url("0.0.0.0:8080"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            public_orchestrator_url("127.0.0.1:9090"),
            "http://127.0.0.1:9090"
        );
        unsafe { env::set_var("API_PUBLIC_BASE", "http://localhost:8080/") };
        assert_eq!(
            public_orchestrator_url("0.0.0.0:1"),
            "http://localhost:8080"
        );
        unsafe { env::remove_var("API_PUBLIC_BASE") };
        unsafe { env::set_var("ORCHESTRATOR_PUBLIC_URL", "http://orch.internal:8081") };
        assert_eq!(
            public_orchestrator_url("0.0.0.0:1"),
            "http://orch.internal:8081"
        );
        unsafe { env::remove_var("ORCHESTRATOR_PUBLIC_URL") };
    }

    #[test]
    fn from_env_snapshots_public_url_onto_config() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe { env::set_var("DATABASE_URL", "postgres://x") };
        unsafe { env::set_var("DATA_DIR", dir.path()) };
        unsafe { env::set_var("API_PUBLIC_BASE", "http://localhost:8080") };
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.orchestrator_public_url, "http://localhost:8080");
        unsafe { env::remove_var("API_PUBLIC_BASE") };
    }
}
