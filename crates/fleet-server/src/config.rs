use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context};
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
        // Unset / empty / whitespace is not a URL. env::var Ok("") must not boot.
        let database_url = match env::var("DATABASE_URL") {
            Ok(s) if !s.trim().is_empty() => s,
            _ => bail!(
                "DATABASE_URL is required and must not be empty or whitespace-only \
                 (Neon, Supabase, or local Postgres)"
            ),
        };

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
        let worker_dist_dir =
            worker_dist_dir_from_raw(env::var("FLEET_WORKER_DIST_DIR").ok().as_deref());

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

/// Unset, empty, or whitespace-only `FLEET_WORKER_DIST_DIR` is `None` (not an error).
fn worker_dist_dir_from_raw(raw: Option<&str>) -> Option<PathBuf> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
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

    fn with_env<T>(pairs: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<(&str, Option<String>)> =
            pairs.iter().map(|(k, _)| (*k, env::var(k).ok())).collect();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
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

    /// Isolate JWT / cookie / encryption so this lock is DATABASE_URL only
    /// (list-jwt-secret-unset-fails-closed owns JWT; do not set AuthMode::None).
    const JWT: &str = "list-database-url-empty-fails-boot-jwt-secret-32+";
    const NOCONNECT_URL: &str =
        "postgres://list-database-url-empty-fails-boot:noconnect@127.0.0.1:1/fleet_from_env_lock";

    fn from_env_for_database_url(database_url: Option<&str>) -> anyhow::Result<ServerConfig> {
        with_env(
            &[
                ("DATABASE_URL", database_url),
                ("JWT_SECRET", Some(JWT)),
                ("INTEGRATIONS_ENCRYPTION_KEY", None),
                ("COOKIE_SAMESITE", Some("Lax")),
                ("COOKIE_SECURE", Some("false")),
                // Isolate bind so a process HOST/PORT cannot mask DATABASE_URL.
                ("HOST", Some("127.0.0.1")),
                ("PORT", Some("18081")),
                ("AUTH_HOST", None),
                ("AUTH_PORT", None),
                ("SANDBOX_BACKEND", None),
            ],
            ServerConfig::from_env,
        )
    }

    fn assert_database_url_rejected(result: anyhow::Result<ServerConfig>, case: &str) {
        match result {
            Ok(cfg) => {
                let url = cfg.database_url.as_str();
                let looks_default = url.contains("postgres://localhost")
                    || url.contains("postgresql://localhost")
                    || url.contains("127.0.0.1");
                panic!(
                    "{case}: DATABASE_URL must fail from_env; must not boot with \
                     empty/whitespace (trimmed_empty={}) or a hardcoded postgres://localhost \
                     default (looks_localhost_default={}, got database_url={:?})",
                    url.trim().is_empty(),
                    looks_default,
                    url
                );
            }
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("DATABASE_URL"),
                    "{case}: error must mention DATABASE_URL, got {msg}"
                );
                assert!(
                    !msg.to_ascii_lowercase().contains("connection refused")
                        && !msg.to_ascii_lowercase().contains("sqlx"),
                    "{case}: must fail at from_env parse, not later on connect, got {msg}"
                );
            }
        }
    }

    #[test]
    fn database_url_unset_empty_and_whitespace_fails_from_env() {
        assert_database_url_rejected(from_env_for_database_url(None), "unset");
        for (case, value) in [
            ("empty", ""),
            ("spaces", "   "),
            ("tabs_newlines", "\t\n"),
            ("mixed_ws", " \t \n "),
        ] {
            assert_database_url_rejected(from_env_for_database_url(Some(value)), case);
        }
    }

    /// Fixed noconnect dummy URL is copied onto ServerConfig. No live Postgres connect.
    #[test]
    fn database_url_explicit_still_parses_bind_cors_auth() {
        let cfg = from_env_for_database_url(Some(NOCONNECT_URL))
            .expect("explicit DATABASE_URL must parse (no live connect)");
        assert_eq!(cfg.database_url, NOCONNECT_URL);
    }

    #[test]
    fn from_env_localhost_host_fails() {
        with_env(
            &[
                ("DATABASE_URL", Some(NOCONNECT_URL)),
                ("JWT_SECRET", Some(JWT)),
                ("INTEGRATIONS_ENCRYPTION_KEY", None),
                ("COOKIE_SAMESITE", Some("Lax")),
                ("COOKIE_SECURE", Some("false")),
                ("HOST", Some("localhost")),
                ("PORT", Some("8080")),
                ("AUTH_HOST", None),
                ("AUTH_PORT", None),
                ("SANDBOX_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env()
                    .expect_err("HOST=localhost is not a SocketAddr host");
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("invalid HOST/PORT"),
                    "error must mention invalid HOST/PORT, got {msg}"
                );
            },
        );
    }

    #[test]
    fn from_env_auth_host_port_fallback() {
        with_env(
            &[
                ("DATABASE_URL", Some(NOCONNECT_URL)),
                ("JWT_SECRET", Some(JWT)),
                ("INTEGRATIONS_ENCRYPTION_KEY", None),
                ("COOKIE_SAMESITE", Some("Lax")),
                ("COOKIE_SECURE", Some("false")),
                ("HOST", None),
                ("PORT", None),
                ("AUTH_HOST", Some("127.0.0.1")),
                ("AUTH_PORT", Some("19090")),
                ("SANDBOX_BACKEND", None),
            ],
            || {
                let cfg = ServerConfig::from_env()
                    .expect("AUTH_HOST/AUTH_PORT must bind when HOST/PORT unset");
                assert_eq!(cfg.bind, "127.0.0.1:19090".parse().expect("fallback bind"));
            },
        );
    }

    #[test]
    fn from_env_cors_origins_default_and_trimmed_list() {
        with_env(
            &[
                ("DATABASE_URL", Some(NOCONNECT_URL)),
                ("JWT_SECRET", Some(JWT)),
                ("INTEGRATIONS_ENCRYPTION_KEY", None),
                ("COOKIE_SAMESITE", Some("Lax")),
                ("COOKIE_SECURE", Some("false")),
                ("HOST", Some("127.0.0.1")),
                ("PORT", Some("18080")),
                ("AUTH_HOST", None),
                ("AUTH_PORT", None),
                ("CORS_ORIGINS", None),
                ("SANDBOX_BACKEND", None),
            ],
            || {
                let cfg = ServerConfig::from_env().expect("unset CORS_ORIGINS must parse");
                assert_eq!(
                    cfg.cors_origins,
                    vec!["http://localhost:3030", "http://127.0.0.1:3030"]
                );
            },
        );
        with_env(
            &[
                ("DATABASE_URL", Some(NOCONNECT_URL)),
                ("JWT_SECRET", Some(JWT)),
                ("INTEGRATIONS_ENCRYPTION_KEY", None),
                ("COOKIE_SAMESITE", Some("Lax")),
                ("COOKIE_SECURE", Some("false")),
                ("HOST", Some("127.0.0.1")),
                ("PORT", Some("18080")),
                ("AUTH_HOST", None),
                ("AUTH_PORT", None),
                ("CORS_ORIGINS", Some(" a.test , , b.test ")),
                ("SANDBOX_BACKEND", None),
            ],
            || {
                let cfg = ServerConfig::from_env().expect("trimmed CORS_ORIGINS must parse");
                assert_eq!(cfg.cors_origins, vec!["a.test", "b.test"]);
            },
        );
    }

    #[test]
    fn empty_or_whitespace_worker_dist_dir_parses_as_none() {
        for (case, value) in [("empty", ""), ("spaces", "   "), ("tabs_newlines", "\t\n")] {
            with_env(
                &[
                    ("DATABASE_URL", Some(NOCONNECT_URL)),
                    ("JWT_SECRET", Some(JWT)),
                    ("INTEGRATIONS_ENCRYPTION_KEY", None),
                    ("COOKIE_SAMESITE", Some("Lax")),
                    ("COOKIE_SECURE", Some("false")),
                    ("HOST", Some("127.0.0.1")),
                    ("PORT", Some("18081")),
                    ("AUTH_HOST", None),
                    ("AUTH_PORT", None),
                    ("SANDBOX_BACKEND", None),
                    ("FLEET_WORKER_DIST_DIR", Some(value)),
                ],
                || {
                    let cfg = ServerConfig::from_env().unwrap_or_else(|err| {
                        panic!(
                            "{case}: empty/whitespace FLEET_WORKER_DIST_DIR must Ok, got {err:#}"
                        )
                    });
                    assert_eq!(
                        cfg.worker_dist_dir, None,
                        "{case}: empty/whitespace FLEET_WORKER_DIST_DIR must be None, not Some empty/CWD"
                    );
                },
            );
        }
    }

    #[test]
    fn set_worker_dist_dir_parses_trimmed_path() {
        for (case, value) in [
            ("set", "/opt/fleet/dist"),
            ("padded", "  /opt/fleet/dist  "),
        ] {
            with_env(
                &[
                    ("DATABASE_URL", Some(NOCONNECT_URL)),
                    ("JWT_SECRET", Some(JWT)),
                    ("INTEGRATIONS_ENCRYPTION_KEY", None),
                    ("COOKIE_SAMESITE", Some("Lax")),
                    ("COOKIE_SECURE", Some("false")),
                    ("HOST", Some("127.0.0.1")),
                    ("PORT", Some("18081")),
                    ("AUTH_HOST", None),
                    ("AUTH_PORT", None),
                    ("SANDBOX_BACKEND", None),
                    ("FLEET_WORKER_DIST_DIR", Some(value)),
                ],
                || {
                    let cfg = ServerConfig::from_env().unwrap_or_else(|err| {
                        panic!("{case}: set FLEET_WORKER_DIST_DIR must Ok, got {err:#}")
                    });
                    assert_eq!(
                        cfg.worker_dist_dir,
                        Some(PathBuf::from("/opt/fleet/dist")),
                        "{case}: set FLEET_WORKER_DIST_DIR must be Some(\"/opt/fleet/dist\") after trim"
                    );
                },
            );
        }
    }
}
