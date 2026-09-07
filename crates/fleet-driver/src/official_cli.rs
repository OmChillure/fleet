//! Official unmodified CLIs on the runner — same family as `fleet-host` `chat_turn`.
//!
//! The control plane never reads `~/.grok/auth.json` (or Claude login). Tokens
//! stay in the CLI's own files on this box.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

pub fn uses_official_cli(harness: &str) -> bool {
    matches!(
        harness.trim().to_ascii_lowercase().as_str(),
        "grok" | "xai" | "claude" | "anthropic" | "ollama"
    )
}

#[derive(Debug, Clone)]
pub struct OfficialOutcome {
    pub ok: bool,
    pub provider: String,
    pub model: Option<String>,
    pub text: String,
    pub command: Vec<String>,
    pub elapsed_sec: f64,
    pub error: Option<String>,
}

pub fn max_turns() -> u32 {
    std::env::var("FLEET_AGENT_MAX_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
        .clamp(1, 48)
}

pub fn grok_argv(prompt_file: &Path, model: &str, effort: Option<&str>, turns: u32) -> Vec<String> {
    let mut args = vec![
        "--prompt-file".into(),
        prompt_file.display().to_string(),
        "--output-format".into(),
        "plain".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--max-turns".into(),
        turns.to_string(),
    ];
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    if let Some(e) = effort {
        args.push("--reasoning-effort".into());
        args.push(e.to_string());
    }
    args
}

/// Agent mode: tools stay enabled (unlike chat_turn's empty `--tools`).
pub fn claude_argv(model: &str, effort: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-p".into(),
        "--output-format".into(),
        "text".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--no-session-persistence".into(),
    ];
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    if let Some(e) = effort {
        args.push("--effort".into());
        args.push(e.to_string());
    }
    args
}

pub fn run(
    harness: &str,
    work_dir: &Path,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    grok_bin: Option<&Path>,
    timeout: Duration,
) -> Result<OfficialOutcome> {
    let provider = match harness.trim().to_ascii_lowercase().as_str() {
        "xai" => "grok",
        "anthropic" => "claude",
        other => other,
    }
    .to_string();
    let model = model.unwrap_or("").trim().to_string();
    let body = with_worker_rules(work_dir, prompt);
    if body.trim().is_empty() {
        bail!("worker prompt is empty");
    }
    let started = Instant::now();
    let (command, text) = match provider.as_str() {
        "grok" => run_grok(&body, &model, effort, grok_bin, timeout, work_dir)?,
        "claude" => run_claude(&body, &model, effort, timeout, work_dir)?,
        "ollama" => run_ollama(&body, &model, timeout, work_dir)?,
        other => bail!("provider `{other}` has no official CLI on this runner"),
    };
    let elapsed_sec = started.elapsed().as_secs_f64();
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(OfficialOutcome {
            ok: false,
            provider,
            model: nonempty_model(&model),
            text,
            command,
            elapsed_sec,
            error: Some("official CLI returned empty stdout".into()),
        });
    }
    Ok(OfficialOutcome {
        ok: true,
        provider,
        model: nonempty_model(&model),
        text,
        command,
        elapsed_sec,
        error: None,
    })
}

fn nonempty_model(model: &str) -> Option<String> {
    let t = model.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

fn with_worker_rules(work_dir: &Path, prompt: &str) -> String {
    let mut out = String::new();
    if let Ok(persona) = fs::read_to_string(work_dir.join("PERSONA.md"))
        && !persona.trim().is_empty()
    {
        out.push_str(persona.trim());
        out.push_str("\n\n");
    }
    out.push_str(prompt.trim());
    out.push_str(
        "\n\n# Fleet worker rules\n\
         You are ONE specialist on a paired runner. Work only inside this directory.\n\
         Shared artifacts go in `work/`. When finished, write `result.md` summarizing \
         what you did, then stop. If you cannot finish, write result.md with blockers.\n\
         Do not request secrets. Do not leave this workspace.\n",
    );
    out
}

fn run_grok(
    prompt: &str,
    model: &str,
    effort: Option<&str>,
    grok_bin: Option<&Path>,
    timeout: Duration,
    cwd: &Path,
) -> Result<(Vec<String>, String)> {
    let fleet = cwd.join(".fleet");
    fs::create_dir_all(&fleet)?;
    let file = fleet.join("agent-prompt.txt");
    fs::write(&file, prompt)?;
    let bin = grok_bin
        .map(|p| p.display().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(grok_bin_default);
    let args = grok_argv(&file, model, effort, max_turns());
    let mut cmd = vec![bin.clone()];
    cmd.extend(args.iter().cloned());
    let out = run_timeout(&bin, &args, None, timeout, cwd)?;
    Ok((cmd, out))
}

fn run_claude(
    prompt: &str,
    model: &str,
    effort: Option<&str>,
    timeout: Duration,
    cwd: &Path,
) -> Result<(Vec<String>, String)> {
    let args = claude_argv(model, effort);
    let mut cmd = vec!["claude".into()];
    cmd.extend(args.iter().cloned());
    let out = run_timeout("claude", &args, Some(prompt), timeout, cwd)?;
    Ok((cmd, out))
}

fn run_ollama(
    prompt: &str,
    model: &str,
    timeout: Duration,
    cwd: &Path,
) -> Result<(Vec<String>, String)> {
    if model.is_empty() {
        bail!("ollama worker needs a model name");
    }
    let args = vec!["run".into(), model.to_string()];
    let mut cmd = vec!["ollama".into()];
    cmd.extend(args.iter().cloned());
    let out = run_timeout("ollama", &args, Some(prompt), timeout, cwd)?;
    Ok((cmd, out))
}

fn grok_bin_default() -> String {
    std::env::var("FLEET_GROK_BIN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let p = PathBuf::from(home).join(".grok/bin/grok");
            p.is_file().then(|| p.display().to_string())
        })
        .unwrap_or_else(|| "grok".into())
}

fn run_timeout(
    bin: &str,
    args: &[String],
    stdin_text: Option<&str>,
    timeout: Duration,
    cwd: &Path,
) -> Result<String> {
    let mut child = Command::new(bin);
    child
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_text.is_some() {
        child.stdin(Stdio::piped());
    } else {
        child.stdin(Stdio::null());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        child.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = child.spawn().map_err(|e| anyhow::anyhow!("{bin}: {e}"))?;
    if let Some(text) = stdin_text
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(text.as_bytes());
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut out, &mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut err, &mut stderr);
                }
                if !status.success() {
                    let err = stderr.trim();
                    if err.is_empty() {
                        bail!("{bin} exited {status}");
                    }
                    bail!("{bin} failed: {err}");
                }
                return Ok(stdout);
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{bin} timed out after {}s", timeout.as_secs());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{bin}: {e}");
            }
        }
    }
}

/// Persist transcript + `result.md` when the CLI did not write one.
pub fn persist_outcome(work_dir: &Path, outcome: &OfficialOutcome) -> Result<()> {
    let dir = work_dir.join("artifacts");
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(work_dir.join("work"))?;
    fs::write(dir.join("transcript.md"), &outcome.text)?;
    let body = json_result(outcome);
    fs::write(dir.join("result.json"), serde_json::to_vec_pretty(&body)?)?;
    let result = work_dir.join("result.md");
    if !result.exists() || fs::read_to_string(&result).map(|s| s.trim().is_empty()).unwrap_or(true)
    {
        let mut md = String::from("# Result\n\n");
        md.push_str(outcome.text.trim());
        md.push('\n');
        fs::write(result, md)?;
    }
    Ok(())
}

fn json_result(outcome: &OfficialOutcome) -> serde_json::Value {
    serde_json::json!({
        "worker": "fleet-driver",
        "ok": outcome.ok,
        "harness": outcome.provider,
        "command": outcome.command,
        "model": outcome.model,
        "elapsed_sec": outcome.elapsed_sec,
        "error": outcome.error,
        "transcript_chars": outcome.text.chars().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_providers() {
        assert!(uses_official_cli("grok"));
        assert!(uses_official_cli("Claude"));
        assert!(uses_official_cli("ollama"));
        assert!(!uses_official_cli("mock"));
        assert!(!uses_official_cli("grok-acp"));
    }

    #[test]
    fn grok_worker_argv_is_agent_not_chat_print() {
        let args = grok_argv(Path::new("/tmp/p.txt"), "grok-4.6", Some("high"), 20);
        assert!(args.contains(&"--prompt-file".into()));
        assert!(args.windows(2).any(|w| w[0] == "--max-turns" && w[1] == "20"));
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "grok-4.6"));
        assert!(!args.iter().any(|a| a == "agent"));
        assert!(!args.iter().any(|a| a == "stdio"));
    }

    #[test]
    fn persist_outcome_writes_result_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let out = OfficialOutcome {
            ok: true,
            provider: "grok".into(),
            model: Some("grok-4.6".into()),
            text: "did the work".into(),
            command: vec!["grok".into()],
            elapsed_sec: 0.1,
            error: None,
        };
        persist_outcome(dir.path(), &out).unwrap();
        let md = std::fs::read_to_string(dir.path().join("result.md")).unwrap();
        assert!(md.contains("did the work"), "{md}");
        assert!(dir.path().join("work").is_dir());
        assert!(dir.path().join("artifacts/transcript.md").is_file());
    }

    #[test]
    fn claude_worker_keeps_tools() {
        let args = claude_argv("sonnet", Some("high"));
        assert!(args.contains(&"-p".into()));
        assert!(!args.windows(2).any(|w| w[0] == "--tools" && w[1].is_empty()));
        assert!(args.windows(2).any(|w| w[0] == "--model" && w[1] == "sonnet"));
    }
}
