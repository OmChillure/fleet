//! Process configuration from environment (design §Config Knobs).

use std::env;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use crate::sandbox::K8sOpts;

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
    /// Kubernetes backend (`SANDBOX_BACKEND=k8s`). Snapshotted at Config build.
    pub k8s: K8sOpts,
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
        require_loopback_for_auth_none(auth_mode, bind_addr)?;
        let data_dir_raw = env_or("DATA_DIR", "./data");
        let data_dir = resolve_data_dir(&data_dir_raw)?;

        let tick_interval = tick_interval_from_env();
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
            sandbox_backend: sandbox_backend_from_env()?,
            stub_duration: duration_secs("STUB_DURATION_S", 3),
            process_worker_argv: process_worker_argv_from_env(),
            k8s: K8sOpts::from_env(),
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
        require_loopback_for_auth_none(auth_mode, &http_addr)?;

        let data_dir_raw = env_or("DATA_DIR", "./data");
        let data_dir = resolve_data_dir(&data_dir_raw)?;

        let tick_interval = tick_interval_from_env();
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
            sandbox_backend: sandbox_backend_from_env()?,
            stub_duration: duration_secs("STUB_DURATION_S", 3),
            process_worker_argv: process_worker_argv_from_env(),
            k8s: K8sOpts::from_env(),
            lock_wait_timeout: lock_wait,
            log_level: env_or("LOG_LEVEL", "info"),
            leadership_mode: "single_process".into(),
            project_filter: env::var("PROJECT_ID").ok().filter(|s| !s.trim().is_empty()),
        })
    }
}

/// Unset / empty → `stub`. Unknown names fail boot (do not silently stub).
///
/// Allowlist matches [`crate::sandbox::build_runtime`]: `stub`, `process`,
/// `container`/`docker`, `k8s`/`kubernetes`, plus `local`/`shell` aliases.
pub fn parse_sandbox_backend(raw: &str) -> Result<String, ConfigError> {
    match raw {
        "stub" | "process" | "local" | "shell" | "docker" | "container" | "k8s" | "kubernetes" => {
            Ok(raw.to_string())
        }
        other => Err(ConfigError::Message(format!(
            "unknown SANDBOX_BACKEND {other:?} (supports stub|process|container|k8s)"
        ))),
    }
}

fn sandbox_backend_from_env() -> Result<String, ConfigError> {
    parse_sandbox_backend(&env_or("SANDBOX_BACKEND", "stub"))
}

fn process_worker_argv_from_env() -> Vec<String> {
    match env::var("PROCESS_WORKER_BIN") {
        Ok(s) if !s.trim().is_empty() => s.split_whitespace().map(|p| p.to_string()).collect(),
        _ => vec!["fleet-driver".into()],
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

/// Background ticker period (`TICK_INTERVAL_S`). Unset → 5s. Zero (including
/// whitespace-padded `0`) would make `tokio::time::sleep(ZERO)` busy-spin
/// advisory locks + spawn/wipe; clamp to ≥1s. Negative/garbage falls through
/// to 5s like other duration knobs. Huge legal values (e.g. 86400) are kept.
fn tick_interval_from_env() -> Duration {
    const DEFAULT_S: u64 = 5;
    const MIN_S: u64 = 1;
    let secs = env::var("TICK_INTERVAL_S")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_S)
        .max(MIN_S);
    Duration::from_secs(secs)
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

/// `AUTH_MODE=none` / `ORCH_AUTH_MODE=none` is dev-only. `AdminAuth` is a no-op
/// when `AuthMode::None`, so a public bind would leave `/v1` unauthenticated.
fn require_loopback_for_auth_none(auth_mode: AuthMode, bind: &str) -> Result<(), ConfigError> {
    if matches!(auth_mode, AuthMode::None) && !is_loopback_addr(bind) {
        return Err(ConfigError::Message(format!(
            "AUTH_MODE=none requires loopback HTTP_ADDR, got {bind:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env tests
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const ENV_KEYS: &[&str] = &[
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
        "ORCHESTRATOR_DATABASE_URL",
        "ORCH_AUTH_MODE",
    ];

    fn clear_relevant() {
        for k in ENV_KEYS {
            unsafe { env::remove_var(k) };
        }
    }

    fn restore_var(key: &str, previous: Option<String>) {
        unsafe {
            match previous {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }
    }

    /// Serialize env mutation and restore the prior process env even if `f` panics.
    /// Drop the mutex before `resume_unwind` so a failing lock does not poison siblings.
    fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
        let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot: Vec<(&str, Option<String>)> =
            ENV_KEYS.iter().map(|&k| (k, env::var(k).ok())).collect();
        clear_relevant();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        for (k, prev) in snapshot {
            restore_var(k, prev);
        }
        drop(g);
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn seed_token_mode(dir: &std::path::Path) {
        unsafe {
            env::set_var(
                "DATABASE_URL",
                "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
            );
            env::set_var("ADMIN_TOKEN", "test-admin-token");
            env::set_var("AUTH_MODE", "token");
            env::set_var("DATA_DIR", dir);
        }
    }

    /// `from_env` / `from_env_for_server` must refuse a spin: fail (mentioning
    /// TICK_INTERVAL) or clamp to a positive minimum (≥1s).
    fn assert_tick_interval_not_spin(result: Result<Config, ConfigError>, raw: &str, via: &str) {
        match result {
            Ok(cfg) => {
                assert!(
                    cfg.tick_interval >= Duration::from_secs(1),
                    "{via} TICK_INTERVAL_S={raw:?} must not yield {:?} \
                     (tokio::time::sleep(ZERO) busy-spins advisory locks + spawn/wipe)",
                    cfg.tick_interval
                );
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("TICK_INTERVAL"),
                    "{via} TICK_INTERVAL_S={raw:?} error must mention TICK_INTERVAL, got {msg:?}"
                );
            }
        }
    }

    #[test]
    fn load_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_relevant();
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            env::set_var(
                "DATABASE_URL",
                "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
            )
        };
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

    /// `AUTH_MODE=none` / `ORCH_AUTH_MODE=none` is dev-only: refuse a non-loopback
    /// bind. `AdminAuth` returns Ok immediately when `AuthMode::None` — `/v1`
    /// has no bearer check. Product boot is `from_env_for_server` (fleet-server).
    fn assert_auth_none_rejected_on_public_bind(
        result: Result<Config, ConfigError>,
        via: &str,
        bind: &str,
    ) {
        match result {
            Ok(cfg) => panic!(
                "{via} AUTH_MODE/ORCH_AUTH_MODE=none bind={bind:?} must fail boot; \
                 must not return Ok with auth_mode={:?} (AdminAuth skips /v1 bearer)",
                cfg.auth_mode
            ),
            Err(e) => {
                let msg = e.to_string();
                let lower = msg.to_ascii_lowercase();
                assert!(
                    lower.contains("auth_mode") && lower.contains("loopback"),
                    "{via} AUTH_MODE/ORCH_AUTH_MODE=none bind={bind:?} error must \
                     mention AUTH_MODE / loopback, got {msg:?}"
                );
            }
        }
    }

    /// Docs (openapi + orch design): none is loopback-only. Drive `from_env`
    /// and `from_env_for_server` — not `is_loopback_addr` / `include_str`.
    #[test]
    fn auth_none_non_loopback_fails_from_env_and_from_env_for_server() {
        with_env_lock(|| {
            let dir = tempfile::tempdir().unwrap();
            unsafe {
                env::set_var("DATABASE_URL", "postgres://x");
                env::set_var("DATA_DIR", dir.path());
                env::set_var("AUTH_MODE", "none");
            }

            for addr in ["0.0.0.0:8080", "[::]:8080", "192.168.1.10:8080", ":8080"] {
                unsafe { env::set_var("HTTP_ADDR", addr) };
                assert_auth_none_rejected_on_public_bind(Config::from_env(), "from_env", addr);
            }

            // AUTH_MODE=none + 127.0.0.1:8080 still Ok (keep auth_none_requires_loopback).
            unsafe { env::set_var("HTTP_ADDR", "127.0.0.1:8080") };
            let loopback =
                Config::from_env().expect("AUTH_MODE=none + 127.0.0.1:8080 must still parse");
            assert_eq!(loopback.auth_mode, AuthMode::None);
            assert_eq!(loopback.http_addr, "127.0.0.1:8080");

            // Control: token + public bind still parses (not every public bind fails).
            unsafe {
                env::set_var("AUTH_MODE", "token");
                env::set_var("HTTP_ADDR", "0.0.0.0:8080");
            }
            let token_public =
                Config::from_env().expect("AUTH_MODE=token + 0.0.0.0:8080 must still parse");
            assert_eq!(token_public.auth_mode, AuthMode::Token);
            assert_eq!(token_public.http_addr, "0.0.0.0:8080");

            // fleet-server: OrchConfig::from_env_for_server(database_url, bind)
            // reads ORCH_AUTH_MODE (not AUTH_MODE) and the bind argument.
            unsafe {
                env::remove_var("AUTH_MODE");
                env::remove_var("HTTP_ADDR");
                env::set_var("ORCH_AUTH_MODE", "none");
            }
            for bind in ["0.0.0.0:8080", "192.168.1.10:8080"] {
                assert_auth_none_rejected_on_public_bind(
                    Config::from_env_for_server("postgres://x", bind),
                    "from_env_for_server",
                    bind,
                );
            }

            let server_loopback = Config::from_env_for_server("postgres://x", "127.0.0.1:8080")
                .expect("from_env_for_server ORCH_AUTH_MODE=none + 127.0.0.1:8080 may Ok");
            assert_eq!(server_loopback.auth_mode, AuthMode::None);
            assert_eq!(server_loopback.http_addr, "127.0.0.1:8080");

            unsafe { env::set_var("ORCH_AUTH_MODE", "token") };
            let server_token = Config::from_env_for_server("postgres://x", "0.0.0.0:8080")
                .expect("ORCH_AUTH_MODE=token + 0.0.0.0:8080 must still parse");
            assert_eq!(server_token.auth_mode, AuthMode::Token);
            assert_eq!(server_token.http_addr, "0.0.0.0:8080");
        });
    }

    #[test]
    fn public_url_rewrites_wildcard_and_prefers_env() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { env::remove_var("ORCHESTRATOR_PUBLIC_URL") };
        unsafe { env::remove_var("API_PUBLIC_BASE") };
        assert_eq!(public_orchestrator_url(":8080"), "http://127.0.0.1:8080");
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

    /// fleet-server sleeps `orch_cfg.tick_interval` then ticks every open project.
    /// `TICK_INTERVAL_S=0` must not become `Duration::ZERO` (busy-loop).
    #[test]
    fn tick_interval_zero_or_huge_must_not_spin() {
        with_env_lock(|| {
            let dir = tempfile::tempdir().unwrap();
            seed_token_mode(dir.path());

            for raw in ["0", "0 "] {
                unsafe { env::set_var("TICK_INTERVAL_S", raw) };
                assert_tick_interval_not_spin(Config::from_env(), raw, "from_env");
                assert_tick_interval_not_spin(
                    Config::from_env_for_server(
                        "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                        "127.0.0.1:18080",
                    ),
                    raw,
                    "from_env_for_server",
                );
            }

            // Huge but legal (one day). Must not panic Duration::from_secs or wrap to ZERO.
            unsafe { env::set_var("TICK_INTERVAL_S", "86400") };
            let daily = std::panic::catch_unwind(|| Config::from_env())
                .expect("TICK_INTERVAL_S=86400 must not panic Duration::from_secs")
                .expect("TICK_INTERVAL_S=86400 is a legal daily interval");
            assert_eq!(daily.tick_interval, Duration::from_secs(86400));
            assert!(daily.tick_interval > Duration::ZERO);
            let daily_server = std::panic::catch_unwind(|| {
                Config::from_env_for_server(
                    "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                    "127.0.0.1:18080",
                )
            })
            .expect("from_env_for_server TICK_INTERVAL_S=86400 must not panic")
            .expect("from_env_for_server TICK_INTERVAL_S=86400 is legal");
            assert_eq!(daily_server.tick_interval, Duration::from_secs(86400));

            // Control: an explicit 5s still parses so this is not "every interval is rejected".
            unsafe { env::set_var("TICK_INTERVAL_S", "5") };
            let five = Config::from_env().expect("TICK_INTERVAL_S=5");
            assert_eq!(five.tick_interval, Duration::from_secs(5));
            let five_server = Config::from_env_for_server(
                "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                "127.0.0.1:18080",
            )
            .expect("from_env_for_server TICK_INTERVAL_S=5");
            assert_eq!(five_server.tick_interval, Duration::from_secs(5));

            // Unset still defaults to 5s (same contract as load_defaults).
            unsafe { env::remove_var("TICK_INTERVAL_S") };
            let unset = Config::from_env().expect("unset TICK_INTERVAL_S");
            assert_eq!(unset.tick_interval, Duration::from_secs(5));
            let unset_server = Config::from_env_for_server(
                "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                "127.0.0.1:18080",
            )
            .expect("from_env_for_server unset TICK_INTERVAL_S");
            assert_eq!(unset_server.tick_interval, Duration::from_secs(5));
        });
    }

    fn assert_unknown_sandbox_backend_rejected(
        result: Result<Config, ConfigError>,
        via: &str,
        raw: &str,
    ) {
        match result {
            Ok(cfg) => panic!(
                "{via} SANDBOX_BACKEND={raw:?} must fail start; must not silently become stub \
                 (got sandbox_backend={:?})",
                cfg.sandbox_backend
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("SANDBOX_BACKEND"),
                    "{via} SANDBOX_BACKEND={raw:?} error must mention SANDBOX_BACKEND, got {msg:?}"
                );
            }
        }
    }

    /// Unknown SANDBOX_BACKEND must refuse boot, not become stub. Allowlist is
    /// process|container|k8s|stub (docker/local/shell/kubernetes stay aliases).
    #[test]
    fn unknown_sandbox_backend_fails_from_env() {
        with_env_lock(|| {
            let dir = tempfile::tempdir().unwrap();
            seed_token_mode(dir.path());

            for raw in [
                "firecracker",
                "vm",
                "qemu",
                "remote",
                "not-a-backend",
                "Process",
                "STUB",
                "none",
            ] {
                unsafe { env::set_var("SANDBOX_BACKEND", raw) };
                assert_unknown_sandbox_backend_rejected(Config::from_env(), "from_env", raw);
                assert_unknown_sandbox_backend_rejected(
                    Config::from_env_for_server(
                        "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                        "127.0.0.1:18080",
                    ),
                    "from_env_for_server",
                    raw,
                );
            }

            for raw in ["stub", "process", "container", "k8s"] {
                unsafe { env::set_var("SANDBOX_BACKEND", raw) };
                let cfg = Config::from_env().unwrap_or_else(|e| {
                    panic!("from_env SANDBOX_BACKEND={raw:?} must still parse: {e}")
                });
                assert_eq!(cfg.sandbox_backend, raw);
                let server = Config::from_env_for_server(
                    "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                    "127.0.0.1:18080",
                )
                .unwrap_or_else(|e| {
                    panic!("from_env_for_server SANDBOX_BACKEND={raw:?} must still parse: {e}")
                });
                assert_eq!(server.sandbox_backend, raw);
            }

            // Documented docker name is the container backend, not unknown.
            unsafe { env::set_var("SANDBOX_BACKEND", "docker") };
            let docker = Config::from_env().expect("SANDBOX_BACKEND=docker is documented");
            assert_eq!(docker.sandbox_backend, "docker");

            unsafe { env::remove_var("SANDBOX_BACKEND") };
            let unset = Config::from_env().expect("unset SANDBOX_BACKEND");
            assert_eq!(unset.sandbox_backend, "stub");
            let unset_server = Config::from_env_for_server(
                "postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable",
                "127.0.0.1:18080",
            )
            .expect("from_env_for_server unset SANDBOX_BACKEND");
            assert_eq!(unset_server.sandbox_backend, "stub");
        });
    }
}
