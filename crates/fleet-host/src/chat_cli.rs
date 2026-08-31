//! Run one chat turn with the official unmodified CLI on this box.
//! Tokens stay in the CLI's own files. Fleet never reads them.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn run_chat_turn(payload: &Value) -> Result<Value> {
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let effort = payload
        .get("effort")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let prompt = payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if prompt.trim().is_empty() {
        bail!("chat_turn missing prompt");
    }
    let timeout = Duration::from_secs(
        payload
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(180)
            .clamp(30, 600),
    );
    let text = match provider.as_str() {
        "grok" | "xai" => run_grok(&prompt, &model, effort.as_deref(), timeout)?,
        "claude" | "anthropic" => run_claude(&prompt, &model, effort.as_deref(), timeout)?,
        "ollama" => run_ollama(&prompt, &model, timeout)?,
        other => bail!("provider `{other}` has no official CLI on this runner"),
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("{provider} CLI returned empty stdout");
    }
    Ok(json!({
        "ok": true,
        "text": text,
        "model": if model.is_empty() { provider.clone() } else { model },
        "provider": provider,
    }))
}

pub fn grok_argv(prompt_file: &Path, model: &str, effort: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--prompt-file".into(),
        prompt_file.display().to_string(),
        "--output-format".into(),
        "plain".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--max-turns".into(),
        "1".into(),
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

fn run_grok(prompt: &str, model: &str, effort: Option<&str>, timeout: Duration) -> Result<String> {
    let dir = tempfile_dir("grok-chat")?;
    let file = dir.join("prompt.txt");
    fs::write(&file, prompt)?;
    let bin = grok_bin();
    let args = grok_argv(&file, model, effort);
    let out = run_timeout(&bin, &args, None, timeout)?;
    let _ = fs::remove_dir_all(&dir);
    Ok(out)
}

/// Print-mode, no tools, no permission prompt. Prompt is stdin (Windows
/// CreateProcess argv is ~8k). `claude models` is not a command.
pub fn claude_argv(model: &str, effort: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-p".into(),
        "--output-format".into(),
        "text".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--no-session-persistence".into(),
        // Official flag: empty list disables tools so this is a reply, not an agent.
        "--tools".into(),
        String::new(),
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

fn run_claude(
    prompt: &str,
    model: &str,
    effort: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let args = claude_argv(model, effort);
    run_timeout("claude", &args, Some(prompt), timeout)
}

fn run_ollama(prompt: &str, model: &str, timeout: Duration) -> Result<String> {
    if model.is_empty() {
        bail!("ollama chat_turn needs a model name");
    }
    run_timeout(
        "ollama",
        &["run".into(), model.to_string()],
        Some(prompt),
        timeout,
    )
}

fn grok_bin() -> String {
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

fn tempfile_dir(prefix: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&base)?;
    Ok(base)
}

fn run_timeout(
    bin: &str,
    args: &[String],
    stdin_text: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let mut child = Command::new(bin);
    child
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_text.is_some() {
        child.stdin(Stdio::piped());
    } else {
        child.stdin(Stdio::null());
    }
    // Windows: do not attach a console. Claude treats a console as interactive
    // and `claude models` / a piped `-p` can wait forever.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        child.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = child.spawn().map_err(|e| anyhow::anyhow!("{bin}: {e}"))?;
    if let Some(text) = stdin_text {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
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
                kill_cli_child(&mut child);
                bail!("{bin} timed out after {}s", timeout.as_secs());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => {
                kill_cli_child(&mut child);
                bail!("{bin}: {e}");
            }
        }
    }
}

fn kill_cli_child(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn grok_argv_includes_model_and_effort() {
        let args = grok_argv(Path::new("/tmp/p.txt"), "grok-4.6", Some("xhigh"));
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "grok-4.6"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--reasoning-effort" && w[1] == "xhigh")
        );
        assert!(args.contains(&"--prompt-file".into()));
    }

    #[test]
    fn claude_argv_is_print_mode_without_tools() {
        let args = claude_argv("sonnet", Some("high"));
        assert!(args.contains(&"-p".into()));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--tools" && w[1].is_empty())
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--permission-mode" && w[1] == "dontAsk")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "sonnet")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--effort" && w[1] == "high")
        );
        assert!(!args.iter().any(|a| a == "hello"));
        assert!(!args.iter().any(|a| a == "models"));
    }
}
