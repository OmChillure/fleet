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
            lock_wait_timeout: lock_wait,
            log_level: env_or("LOG_LEVEL", "info"),
            leadership_mode: "single_process".into(),
            project_filter: env::var("PROJECT_ID").ok().filter(|s| !s.trim().is_empty()),
        })
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
    env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(def)
}

fn duration_secs(k: &str, def: u64) -> Duration {
    Duration::from_secs(env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(def))
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
    std::fs::create_dir_all(&p)
        .map_err(|e| ConfigError::Message(format!("DATA_DIR create {}: {e}", p.display())))?;
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

    fn with_env<T>(pairs: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<(&str, Option<String>)> =
            pairs.iter().map(|(k, _)| (*k, env::var(k).ok())).collect();
        // SAFETY: held under ENV_LOCK; restored before unlock / panic resume.
        unsafe {
            for (k, v) in pairs {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            for (k, v) in previous {
                match v {
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

    #[test]
    fn admin_token_optional_in_token_mode() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_str().expect("utf-8 DATA_DIR").to_owned();
        let cfg = with_env(
            &[("ADMIN_TOKEN", None), ("DATA_DIR", Some(data_dir.as_str()))],
            || {
                Config::from_env_for_server("postgres://x", "127.0.0.1:8080")
                    .expect("ADMIN_TOKEN is optional")
            },
        );
        assert!(cfg.admin_token.is_empty());
    }

    #[test]
    fn from_env_copies_set_admin_token() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_str().expect("utf-8 DATA_DIR").to_owned();
        let cfg = with_env(
            &[
                ("ADMIN_TOKEN", Some("secret-token")),
                ("DATA_DIR", Some(data_dir.as_str())),
            ],
            || {
                Config::from_env_for_server("postgres://x", "127.0.0.1:8080")
                    .expect("set ADMIN_TOKEN")
            },
        );
        assert_eq!(cfg.admin_token, "secret-token");
    }

    #[test]
    fn public_url_rewrites_port_only_bind() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", None),
                ("API_PUBLIC_BASE", None),
            ],
            || public_orchestrator_url(":8080"),
        );
        assert_eq!(got, "http://127.0.0.1:8080");
    }

    #[test]
    fn public_url_rewrites_ipv4_unspecified_bind() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", None),
                ("API_PUBLIC_BASE", None),
            ],
            || public_orchestrator_url("0.0.0.0:8080"),
        );
        assert_eq!(got, "http://127.0.0.1:8080");
    }

    #[test]
    fn public_url_preserves_explicit_loopback_bind() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", None),
                ("API_PUBLIC_BASE", None),
            ],
            || public_orchestrator_url("127.0.0.1:9090"),
        );
        assert_eq!(got, "http://127.0.0.1:9090");
    }

    #[test]
    fn public_url_rewrites_ipv6_unspecified_bind() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", None),
                ("API_PUBLIC_BASE", None),
            ],
            || public_orchestrator_url("[::]:8080"),
        );
        assert_eq!(got, "http://127.0.0.1:8080");
    }

    #[test]
    fn public_url_prefers_api_public_base_stripping_trailing_slash() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", None),
                ("API_PUBLIC_BASE", Some("http://localhost:8080/")),
            ],
            || public_orchestrator_url("0.0.0.0:1"),
        );
        assert_eq!(got, "http://localhost:8080");
    }

    #[test]
    fn public_url_prefers_orchestrator_public_url() {
        let got = with_env(
            &[
                ("ORCHESTRATOR_PUBLIC_URL", Some("http://orch.internal:8081")),
                ("API_PUBLIC_BASE", None),
            ],
            || public_orchestrator_url("0.0.0.0:1"),
        );
        assert_eq!(got, "http://orch.internal:8081");
    }

    #[test]
    fn public_url_prefers_orchestrator_public_url_when_both_env_set() {
        let got = with_env(
            &[
                (
                    "ORCHESTRATOR_PUBLIC_URL",
                    Some("http://api.agent-fleet.svc.cluster.local:8080"),
                ),
                ("API_PUBLIC_BASE", Some("http://localhost:8080")),
            ],
            || public_orchestrator_url("0.0.0.0:8080"),
        );
        assert_eq!(got, "http://api.agent-fleet.svc.cluster.local:8080");
    }

    #[test]
    fn public_url_blank_env_falls_through_to_bind_rewrite() {
        for blank in ["", "   ", "/"] {
            let got = with_env(
                &[
                    ("ORCHESTRATOR_PUBLIC_URL", Some(blank)),
                    ("API_PUBLIC_BASE", Some(blank)),
                ],
                || public_orchestrator_url("0.0.0.0:8080"),
            );
            assert_eq!(
                got,
                "http://127.0.0.1:8080",
                "blank env {blank:?} must fall through to bind rewrite"
            );
        }
    }
}
