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
/// * `grok` → `grok agent --no-leader [--always-approve] [--reasoning-effort E] [-m MODEL] stdio`
/// * `claude` → official `claude -p` with tools (not chat's no-tools print)
/// * `ollama` → local HTTP tool loop (no ACP)
pub fn resolve(
    name: &str,
    harness_cmd: Option<&str>,
    model: Option<&str>,
    auto_approve: bool,
    mock_bin: Option<&Path>,
    grok_bin: Option<&Path>,
    reasoning_effort: Option<&str>,
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
            let mut args = vec!["agent".into(), "--no-leader".into()];
            if auto_approve {
                args.push("--always-approve".into());
            }
            if let Some(e) = reasoning_effort.map(str::trim).filter(|s| !s.is_empty()) {
                args.push("--reasoning-effort".into());
                args.push(e.to_string());
            }
            if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
                args.push("-m".into());
                args.push(m.to_string());
            }
            args.push("stdio".into());
            Ok(HarnessSpec {
                name: "grok".into(),
                command: find_grok(grok_bin),
                args,
            })
        }
        "claude" | "anthropic" => {
            let mut args = vec![
                "-p".into(),
                "--output-format".into(),
                "text".into(),
                "--permission-mode".into(),
                if auto_approve {
                    "dontAsk".into()
                } else {
                    "acceptEdits".into()
                },
                "--no-session-persistence".into(),
            ];
            if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
                args.push("--model".into());
                args.push(m.to_string());
            }
            if let Some(e) = reasoning_effort.map(str::trim).filter(|s| !s.is_empty()) {
                args.push("--effort".into());
                args.push(e.to_string());
            }
            Ok(HarnessSpec {
                name: "claude".into(),
                command: "claude".into(),
                args,
            })
        }
        "ollama" => Ok(HarnessSpec {
            name: "ollama".into(),
            command: "ollama".into(),
            args: Vec::new(),
        }),
        other => bail!("unknown harness {other:?}; expected mock|grok|claude|ollama"),
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

fn find_grok(explicit: Option<&Path>) -> String {
    if let Some(p) = explicit {
        return p.display().to_string();
    }
    if let Ok(p) = std::env::var("FLEET_GROK_BIN")
        && !p.trim().is_empty()
    {
        return p;
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(home).join(".grok/bin/grok");
        if candidate.is_file() {
            return candidate.display().to_string();
        }
    }
    "grok".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_argv_with_model_and_approve() {
        let h = resolve(
            "grok",
            None,
            Some("grok-4"),
            true,
            None,
            Some(Path::new("/usr/bin/grok")),
            Some("low"),
        )
        .unwrap();
        assert_eq!(h.name, "grok");
        assert_eq!(h.command, "/usr/bin/grok");
        assert_eq!(
            h.args,
            vec![
                "agent",
                "--no-leader",
                "--always-approve",
                "--reasoning-effort",
                "low",
                "-m",
                "grok-4",
                "stdio"
            ]
        );
    }

    #[test]
    fn grok_argv_no_approve() {
        let h = resolve(
            "grok",
            None,
            None,
            false,
            None,
            Some(Path::new("grok")),
            None,
        )
        .unwrap();
        assert_eq!(h.args, vec!["agent", "--no-leader", "stdio"]);
    }

    #[test]
    fn override_cmd() {
        let h = resolve(
            "grok",
            Some("docker run -i agent-fleet-grok:alpine"),
            None,
            true,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(h.command, "docker");
        assert_eq!(h.args, vec!["run", "-i", "agent-fleet-grok:alpine"]);
    }

    #[test]
    fn unknown_harness_errs() {
        assert!(resolve("opencode", None, None, true, None, None, None).is_err());
    }

    #[test]
    fn claude_print_keeps_tools() {
        let h = resolve("claude", None, Some("sonnet"), true, None, None, Some("high"))
            .unwrap();
        assert_eq!(h.name, "claude");
        assert_eq!(h.command, "claude");
        assert!(h.args.contains(&"-p".into()));
        assert!(!h.args.windows(2).any(|w| w[0] == "--tools" && w[1].is_empty()));
        assert!(h.args.windows(2).any(|w| w[0] == "--model" && w[1] == "sonnet"));
    }

    #[test]
    fn ollama_is_named_loop() {
        let h = resolve("ollama", None, Some("llama3.2"), true, None, None, None).unwrap();
        assert_eq!(h.name, "ollama");
        assert!(h.args.is_empty());
    }
}
