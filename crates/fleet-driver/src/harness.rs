//! Resolve how to spawn the ACP agent process.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Resolve harness argv.
///
/// * `FLEET_HARNESS_CMD` (whitespace-split) overrides command+args entirely.
/// * `mock` looks next to the current executable, then PATH.
/// * `grok` → `grok agent [--always-approve] [-m MODEL] stdio`
pub fn resolve(
    name: &str,
    harness_cmd: Option<&str>,
    model: Option<&str>,
    auto_approve: bool,
    mock_bin: Option<&Path>,
) -> Result<HarnessSpec> {
    let name = name.trim().to_ascii_lowercase();
    if let Some(raw) = harness_cmd.map(str::trim).filter(|s| !s.is_empty()) {
        let mut parts = raw.split_whitespace().map(|s| s.to_string());
        let command = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("FLEET_HARNESS_CMD is empty"))?;
        return Ok(HarnessSpec {
            name: if name.is_empty() {
                "custom".into()
            } else {
                name
            },
            command,
            args: parts.collect(),
        });
    }

    match name.as_str() {
        "" | "mock" => {
            let command = mock_command(mock_bin);
            Ok(HarnessSpec {
                name: "mock".into(),
                command,
                args: vec![],
            })
        }
        "grok" => {
            let mut args = vec!["agent".into()];
            if auto_approve {
                args.push("--always-approve".into());
            }
            if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
                args.push("-m".into());
                args.push(m.to_string());
            }
            args.push("stdio".into());
            Ok(HarnessSpec {
                name: "grok".into(),
                command: "grok".into(),
                args,
            })
        }
        other => bail!("unknown harness {other:?}; expected mock|grok"),
    }
}

fn mock_command(explicit: Option<&Path>) -> String {
    if let Some(p) = explicit {
        return p.display().to_string();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("mock-acp-agent");
        if sibling.is_file() {
            return sibling.display().to_string();
        }
        #[cfg(windows)]
        {
            let sibling = dir.join("mock-acp-agent.exe");
            if sibling.is_file() {
                return sibling.display().to_string();
            }
        }
    }
    "mock-acp-agent".into()
}

/// Default mock binary next to `fleet-driver` (used by tests too).
pub fn mock_bin_path() -> PathBuf {
    PathBuf::from(mock_command(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_argv_with_model_and_approve() {
        let h = resolve("grok", None, Some("grok-4"), true, None).unwrap();
        assert_eq!(h.name, "grok");
        assert_eq!(h.command, "grok");
        assert_eq!(
            h.args,
            vec!["agent", "--always-approve", "-m", "grok-4", "stdio"]
        );
    }

    #[test]
    fn grok_argv_no_approve() {
        let h = resolve("grok", None, None, false, None).unwrap();
        assert_eq!(h.args, vec!["agent", "stdio"]);
    }

    #[test]
    fn override_cmd() {
        let h = resolve(
            "grok",
            Some("docker run -i agent-fleet-grok:alpine"),
            None,
            true,
            None,
        )
        .unwrap();
        assert_eq!(h.command, "docker");
        assert_eq!(h.args, vec!["run", "-i", "agent-fleet-grok:alpine"]);
    }

    #[test]
    fn unknown_harness_errs() {
        assert!(resolve("opencode", None, None, true, None).is_err());
    }
}
