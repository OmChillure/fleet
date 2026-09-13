//! Driver configuration from `FLEET_*` env (orchestrator spawn contract).

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub api_base: String,
    pub project_id: String,
    pub session_id: String,
    pub board_task_id: String,
    pub plan_task_id: String,
    pub token: String,
    pub work_dir: PathBuf,
    /// `mock` | `grok` | `claude` | `ollama` (default `mock`). `grok-acp` keeps ACP.
    pub harness: String,
    pub model: Option<String>,
    pub timeout: Duration,
    pub heartbeat_interval: Duration,
    /// Default true (v1). Set `FLEET_AUTO_APPROVE=0` to deny permission requests.
    pub auto_approve: bool,
    /// Optional whitespace-split argv override (`docker run -i …`).
    pub harness_cmd: Option<String>,
    /// Override path to `mock-acp-agent`.
    pub mock_bin: Option<PathBuf>,
    /// Override path to the `grok` CLI.
    pub grok_bin: Option<PathBuf>,
    /// Passed as `grok agent --reasoning-effort`.
    pub reasoning_effort: Option<String>,
    /// Write `grok-acp.debug.log` in the work dir.
    pub grok_debug: bool,
    /// Project git remote (clone URL). None = skip provision clone.
    pub repo_url: Option<String>,
    pub git_ref: String,
}

impl DriverConfig {
    pub fn from_env() -> Result<Self> {
        let require = |k: &str| env::var(k).map_err(|_| anyhow!("missing required env {k}"));
        let api_base = env::var("FLEET_ORCHESTRATOR_URL")
            .or_else(|_| env::var("FLEET_API_BASE"))
            .map_err(|_| anyhow!("missing FLEET_ORCHESTRATOR_URL or FLEET_API_BASE"))?
            .trim_end_matches('/')
            .to_string();
        let work_dir = PathBuf::from(require("FLEET_WORK_DIR")?);
        let board_task_id = env::var("FLEET_BOARD_TASK_ID")
            .or_else(|_| env::var("FLEET_TASK_ID"))
            .map_err(|_| anyhow!("missing FLEET_BOARD_TASK_ID / FLEET_TASK_ID"))?;
        let hb_s: u64 = env::var("FLEET_HEARTBEAT_INTERVAL_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2)
            .max(1);
        let timeout_s: u64 = env::var("FLEET_TASK_TIMEOUT_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600)
            .max(1);
        let harness = env::var("FLEET_HARNESS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "mock".into())
            .to_ascii_lowercase();
        Ok(Self {
            api_base,
            project_id: require("FLEET_PROJECT_ID")?,
            session_id: require("FLEET_SESSION_ID")?,
            board_task_id,
            plan_task_id: env::var("FLEET_PLAN_TASK_ID").unwrap_or_default(),
            token: require("FLEET_WORKER_TOKEN")?,
            work_dir,
            harness,
            model: env::var("FLEET_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            timeout: Duration::from_secs(timeout_s),
            heartbeat_interval: Duration::from_secs(hb_s),
            auto_approve: parse_env_bool_default_true(
                env::var("FLEET_AUTO_APPROVE").ok().as_deref(),
            ),
            harness_cmd: env::var("FLEET_HARNESS_CMD")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            mock_bin: env::var("FLEET_MOCK_ACP_BIN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
            grok_bin: env::var("FLEET_GROK_BIN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
            reasoning_effort: env::var("FLEET_REASONING_EFFORT")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            grok_debug: parse_env_bool_default_false(env::var("FLEET_GROK_DEBUG").ok().as_deref()),
            repo_url: env::var("FLEET_REPO_URL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            git_ref: env::var("FLEET_GIT_REF")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "main".into()),
        })
    }
}

/// `FLEET_AUTO_APPROVE`: unset / empty → true; 0/false/no/off → false.
pub fn parse_env_bool_default_true(raw: Option<&str>) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => false,
            "1" | "true" | "yes" | "on" => true,
            _ => true,
        },
    }
}

/// Unset / empty / unknown → false.
pub fn parse_env_bool_default_false(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
            .unwrap_or(""),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_approve_defaults_true() {
        for (raw, want) in [
            (None, true),
            (Some(""), true),
            (Some("1"), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some("no"), false),
            (Some("off"), false),
        ] {
            assert_eq!(parse_env_bool_default_true(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn parse_env_bool_default_true_no_and_off_are_false() {
        assert_eq!(parse_env_bool_default_true(Some("no")), false, r#"Some("no")"#);
        assert_eq!(parse_env_bool_default_true(Some("OFF")), false, r#"Some("OFF")"#);
    }

    #[test]
    fn parse_env_bool_default_true_unknown_and_whitespace_stay_true() {
        assert_eq!(
            parse_env_bool_default_true(Some("maybe")),
            true,
            r#"Some("maybe")"#
        );
        assert_eq!(
            parse_env_bool_default_true(Some("  ")),
            true,
            r#"Some("  ")"#
        );
    }

    #[test]
    fn parse_env_bool_default_false_unset_empty_unknown_are_false() {
        assert_eq!(parse_env_bool_default_false(None), false, "None");
        assert_eq!(parse_env_bool_default_false(Some("")), false, r#"Some("")"#);
        assert_eq!(
            parse_env_bool_default_false(Some("maybe")),
            false,
            r#"Some("maybe")"#
        );
    }
}
