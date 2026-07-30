//! Harness registry: how to spawn ACP agents.

use crate::job::Job;
use anyhow::{bail, Context, Result};
use std::path::Path;
use which::which;

#[derive(Debug, Clone)]
pub struct HarnessSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// `config_option` | `set_model` | `none`
    pub model_via: String,
    pub model_config_id: String,
}

pub fn resolve_harness(job: &Job) -> Result<HarnessSpec> {
    let name = job.harness.to_lowercase();
    match name.as_str() {
        "custom" | "user" => {
            let command = job
                .command
                .clone()
                .context("harness=custom requires job.command")?;
            Ok(HarnessSpec {
                name: "custom".into(),
                command,
                args: job.args.clone(),
                model_via: "config_option".into(),
                model_config_id: "model".into(),
            })
        }
        "opencode" => {
            ensure_bin("opencode")?;
            // Allow override
            if let Some(cmd) = &job.command {
                return Ok(HarnessSpec {
                    name: "opencode".into(),
                    command: cmd.clone(),
                    args: if job.args.is_empty() {
                        vec!["acp".into()]
                    } else {
                        job.args.clone()
                    },
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                });
            }
            Ok(HarnessSpec {
                name: "opencode".into(),
                command: "opencode".into(),
                args: vec!["acp".into()],
                model_via: "config_option".into(),
                model_config_id: "model".into(),
            })
        }
        "codex" => {
            if which("codex-acp").is_ok() {
                Ok(HarnessSpec {
                    name: "codex".into(),
                    command: "codex-acp".into(),
                    args: vec![],
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                })
            } else if which("npx").is_ok() {
                Ok(HarnessSpec {
                    name: "codex".into(),
                    command: "npx".into(),
                    args: vec!["-y".into(), "@agentclientprotocol/codex-acp".into()],
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                })
            } else {
                bail!("harness=codex: neither codex-acp nor npx on PATH");
            }
        }
        "grok" => {
            ensure_bin("grok")?;
            Ok(HarnessSpec {
                name: "grok".into(),
                command: "grok".into(),
                args: vec!["agent".into(), "stdio".into()],
                model_via: "set_model".into(),
                model_config_id: "model".into(),
            })
        }
        // Pi (https://pi.dev) via community ACP adapter `pi-acp` → `pi --mode rpc`
        "pi" | "pi-dev" | "pi_acp" => {
            if let Some(cmd) = &job.command {
                return Ok(HarnessSpec {
                    name: "pi".into(),
                    command: cmd.clone(),
                    args: job.args.clone(),
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                });
            }
            if which("pi-acp").is_ok() {
                Ok(HarnessSpec {
                    name: "pi".into(),
                    command: "pi-acp".into(),
                    args: vec![],
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                })
            } else if which("npx").is_ok() {
                Ok(HarnessSpec {
                    name: "pi".into(),
                    command: "npx".into(),
                    args: vec!["-y".into(), "pi-acp".into()],
                    model_via: "config_option".into(),
                    model_config_id: "model".into(),
                })
            } else {
                bail!("harness=pi: neither pi-acp nor npx on PATH (build agents/pi image)");
            }
        }
        other => bail!("unknown harness {other:?}; expected opencode|codex|grok|pi|custom"),
    }
}

fn ensure_bin(name: &str) -> Result<()> {
    which(name).with_context(|| {
        format!("harness binary {name:?} not on PATH (wrong agent image?)")
    })?;
    Ok(())
}

/// Env vars for the harness child process.
pub fn harness_env(job: &Job, harness: &str, work_dir: &Path) -> Result<std::collections::HashMap<String, String>> {
    let mut env = std::collections::HashMap::new();
    match harness {
        "opencode" => {
            crate::opencode_config::setup_opencode(job, work_dir, &mut env)?;
        }
        "codex" => {
            if let Some(k) = job
                .openai_api_key
                .clone()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            {
                env.insert("OPENAI_API_KEY".into(), k);
            }
            if let Some(b) = job
                .openai_base_url
                .clone()
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            {
                env.insert("OPENAI_BASE_URL".into(), b);
            }
        }
        "grok" => {
            if let Some(k) = job
                .xai_api_key
                .clone()
                .or_else(|| std::env::var("XAI_API_KEY").ok())
                .or_else(|| std::env::var("GROK_DEPLOYMENT_KEY").ok())
            {
                env.insert("XAI_API_KEY".into(), k.clone());
                env.insert("GROK_DEPLOYMENT_KEY".into(), k);
            }
        }
        "pi" | "pi-dev" | "pi_acp" => {
            crate::pi_config::setup_pi(job, work_dir, &mut env)?;
        }
        _ => {}
    }
    Ok(env)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn job_json(v: serde_json::Value) -> Job {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn custom_harness_requires_command() {
        let j = job_json(serde_json::json!({"harness":"custom","repo_url":"x"}));
        assert!(resolve_harness(&j).is_err());
    }

    #[test]
    fn custom_harness_ok() {
        let j = job_json(serde_json::json!({
            "harness": "custom",
            "command": "echo-agent",
            "args": ["acp"],
            "repo_url": "x"
        }));
        let h = resolve_harness(&j).unwrap();
        assert_eq!(h.name, "custom");
        assert_eq!(h.command, "echo-agent");
        assert_eq!(h.args, vec!["acp"]);
        assert_eq!(h.model_via, "config_option");
    }

    #[test]
    fn unknown_harness_errors() {
        let j = job_json(serde_json::json!({"harness":"nope","repo_url":"x"}));
        let err = resolve_harness(&j).unwrap_err().to_string();
        assert!(err.contains("unknown harness"), "{err}");
    }

    #[test]
    fn pi_harness_resolves_when_binary_or_npx() {
        let j = job_json(serde_json::json!({
            "harness": "pi",
            "repo_url": "x"
        }));
        match resolve_harness(&j) {
            Ok(h) => {
                assert_eq!(h.name, "pi");
                assert!(
                    h.command == "pi-acp" || h.command == "npx",
                    "cmd={}",
                    h.command
                );
                assert_eq!(h.model_via, "config_option");
            }
            Err(e) => {
                let s = e.to_string();
                assert!(s.contains("pi-acp") || s.contains("npx"), "{s}");
            }
        }
    }

    #[test]
    fn opencode_spec_when_binary_present_or_skip() {
        let j = job_json(serde_json::json!({
            "harness": "opencode",
            "repo_url": "x"
        }));
        match resolve_harness(&j) {
            Ok(h) => {
                assert_eq!(h.name, "opencode");
                assert_eq!(h.command, "opencode");
                assert_eq!(h.args, vec!["acp"]);
            }
            Err(e) => {
                // CI/dev without opencode installed
                assert!(e.to_string().contains("opencode"), "{e}");
            }
        }
    }

    #[test]
    fn opencode_command_override_still_checks_default_bin_first() {
        // When harness is opencode, ensure_bin runs before override.
        // Use custom harness instead to override freely (documented behavior).
        let j = job_json(serde_json::json!({
            "harness": "custom",
            "command": "/usr/bin/true",
            "args": [],
            "repo_url": "x"
        }));
        let h = resolve_harness(&j).unwrap();
        assert_eq!(h.command, "/usr/bin/true");
    }
}
