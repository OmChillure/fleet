//! Run one chat turn with the official unmodified CLI on this box.
//! Tokens stay in the CLI's own files. Fleet never reads them.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn run_chat_turn(payload: &Value, on_progress: &mut dyn FnMut(&str)) -> Result<Value> {
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
    let max_turns = payload
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(32)
        .clamp(1, 128) as u32;
    let text = match provider.as_str() {
        "grok" | "xai" => run_grok(
            &prompt,
            &model,
            effort.as_deref(),
            max_turns,
            timeout,
            on_progress,
        )?,
        "claude" | "anthropic" => run_claude(&prompt, &model, effort.as_deref(), timeout)?,
        "openai" | "codex" => run_openai(&prompt, &model, timeout)?,
        "gemini" | "google" => run_gemini(&prompt, &model, timeout)?,
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

pub fn grok_argv(
    prompt_file: &Path,
    model: &str,
    effort: Option<&str>,
    max_turns: u32,
) -> Vec<String> {
    let turns = max_turns.clamp(1, 128);
    let mut args = vec![
        "--prompt-file".into(),
        prompt_file.display().to_string(),
        "--output-format".into(),
        "streaming-json".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        // `--prompt-file` is one *user* prompt. `--max-turns 1` is one *agent*
        // step: a first-turn tool call exits "Max turns reached". Workspace
        // Settings `chat_max_turns` (default 32) is the live cap.
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

fn run_grok(
    prompt: &str,
    model: &str,
    effort: Option<&str>,
    max_turns: u32,
    timeout: Duration,
    on_progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let dir = tempfile_dir("grok-chat")?;
    let file = dir.join("prompt.txt");
    fs::write(&file, prompt)?;
    let bin = grok_bin();
    let args = grok_argv(&file, model, effort, max_turns);
    let out = run_grok_stream(&bin, &args, timeout, on_progress)?;
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

fn run_openai(prompt: &str, model: &str, timeout: Duration) -> Result<String> {
    let mut args = vec!["exec".into(), "--skip-git-repo-check".into()];
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    run_timeout("codex", &args, Some(prompt), timeout)
}

fn run_gemini(prompt: &str, model: &str, timeout: Duration) -> Result<String> {
    let mut args = vec!["-p".into()];
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    run_timeout("gemini", &args, Some(prompt), timeout)
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

/// `streaming-json` lines from `grok`. Tool rows are reported as they start.
/// The reply is the text after the last native tool call, or all text when
/// the model did not call a tool (including a fleet `tool_calls` JSON blob).
#[derive(Default)]
struct GrokStreamAccum {
    before_tool: String,
    after_tool: String,
    seen_tool: bool,
    stop_reason: Option<String>,
    non_json: String,
    last_progress: String,
}

impl GrokStreamAccum {
    fn on_line(&mut self, line: &str, on_progress: &mut dyn FnMut(&str)) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match json_type_field(line) {
            Some("tool_call") => {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    return;
                };
                self.seen_tool = true;
                self.after_tool.clear();
                if let Some(label) = tool_call_label(&v) {
                    self.emit(&label, on_progress);
                }
            }
            Some("text") => {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    return;
                };
                let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                if self.seen_tool {
                    self.after_tool.push_str(data);
                } else {
                    self.before_tool.push_str(data);
                }
            }
            Some("end") => {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    self.stop_reason = v
                        .get("stopReason")
                        .and_then(|s| s.as_str())
                        .map(str::to_string);
                }
            }
            Some("tool_call_update" | "available_commands" | "usage") => {}
            _ => {
                if !self.non_json.is_empty() {
                    self.non_json.push('\n');
                }
                self.non_json.push_str(line);
            }
        }
    }

    fn emit(&mut self, text: &str, on_progress: &mut dyn FnMut(&str)) {
        if self.last_progress == text {
            return;
        }
        self.last_progress = text.to_string();
        on_progress(text);
    }

    fn answer(&self) -> Result<String> {
        let preferred = if self.seen_tool && !self.after_tool.trim().is_empty() {
            self.after_tool.trim()
        } else {
            self.before_tool.trim()
        };
        if !preferred.is_empty() {
            return Ok(preferred.to_string());
        }
        let raw = self.non_json.trim();
        if !raw.is_empty() {
            return Ok(raw.to_string());
        }
        if let Some(reason) = self.stop_reason.as_deref().filter(|s| *s != "end_turn") {
            bail!("grok stopped: {reason}");
        }
        bail!("grok CLI returned empty stdout");
    }
}

fn json_type_field(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix('{')?.trim_start();
    let rest = rest.strip_prefix("\"type\"")?.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn tool_call_label(v: &Value) -> Option<String> {
    let name = v
        .get("toolName")
        .or_else(|| v.get("title"))
        .or_else(|| v.get("name"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let detail = v
        .get("rawInput")
        .and_then(tool_input_detail)
        .unwrap_or_default();
    let mut label = format!("Using {name}…");
    if !detail.is_empty() {
        label.push(' ');
        label.push_str(&detail);
    }
    Some(clip_chars(&label, 180))
}

fn tool_input_detail(raw: &Value) -> Option<String> {
    for key in [
        "description",
        "command",
        "path",
        "file_path",
        "pattern",
        "query",
    ] {
        if let Some(s) = raw.get(key).and_then(|v| v.as_str()) {
            let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if !flat.is_empty() {
                return Some(clip_chars(&flat, 80));
            }
        }
    }
    None
}

fn clip_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars.into_iter().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn run_grok_stream(
    bin: &str,
    args: &[String],
    timeout: Duration,
    on_progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut child = Command::new(bin);
    child
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        child.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = child.spawn().map_err(|e| anyhow::anyhow!("{bin}: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("{bin}: missing stdout"))?;
    let stderr = child.stderr.take();
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_task = stderr.map(|mut err| {
        let buf = stderr_buf.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            if let Ok(mut g) = buf.lock() {
                *g = s;
            }
        })
    });
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim_end().to_string()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let start = Instant::now();
    let mut accum = GrokStreamAccum::default();
    let mut timed_out = false;
    let status = 'wait: loop {
        if start.elapsed() >= timeout {
            timed_out = true;
            kill_cli_child(&mut child);
            break 'wait None;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => accum.on_line(&line, on_progress),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = child.try_wait() {
                    break 'wait Some(status);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Stdout closed. Keep the kill timeout if the process is still alive.
                loop {
                    if start.elapsed() >= timeout {
                        timed_out = true;
                        kill_cli_child(&mut child);
                        break 'wait None;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break 'wait Some(status),
                        Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                        Err(e) => bail!("{bin}: {e}"),
                    }
                }
            }
        }
    };
    let _ = reader.join();
    while let Ok(line) = rx.try_recv() {
        accum.on_line(&line, on_progress);
    }
    if let Some(task) = stderr_task {
        let _ = task.join();
    }
    if timed_out {
        bail!("{bin} timed out after {}s", timeout.as_secs());
    }
    let Some(status) = status else {
        bail!("{bin} timed out after {}s", timeout.as_secs());
    };
    if !status.success() {
        let err = stderr_buf
            .lock()
            .map(|g| g.trim().to_string())
            .unwrap_or_default();
        if err.is_empty() {
            bail!("{bin} exited {status}");
        }
        bail!("{bin} failed: {err}");
    }
    accum.answer()
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
        let mut kill = Command::new("taskkill");
        crate::platform::hide_console(&mut kill);
        let _ = kill
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
        let args = grok_argv(Path::new("/tmp/p.txt"), "grok-4.6", Some("xhigh"), 32);
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "grok-4.6"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--reasoning-effort" && w[1] == "xhigh")
        );
    }

    #[test]
    fn grok_argv_omits_blank_model_and_none_effort() {
        let args = grok_argv(Path::new("/tmp/p.txt"), "", None, 32);
        assert!(!args.iter().any(|a| a == "-m"));
        assert!(!args.iter().any(|a| a == "--reasoning-effort"));
    }

    #[test]
    fn grok_argv_prompt_file_path_and_noninteractive_flags() {
        let path = Path::new("/tmp/p.txt");
        let args = grok_argv(path, "", None, 32);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--prompt-file" && w[1] == path.display().to_string())
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "streaming-json")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--permission-mode" && w[1] == "dontAsk")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--max-turns" && w[1] == "32")
        );
    }

    #[test]
    fn grok_argv_uses_workspace_max_turns() {
        let args = grok_argv(Path::new("/tmp/p.txt"), "", None, 8);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--max-turns" && w[1] == "8"),
            "{args:?}"
        );
        let clamped = grok_argv(Path::new("/tmp/p.txt"), "", None, 0);
        assert!(
            clamped
                .windows(2)
                .any(|w| w[0] == "--max-turns" && w[1] == "1"),
            "{clamped:?}"
        );
    }

    #[test]
    fn grok_argv_model_without_effort() {
        let args = grok_argv(Path::new("/tmp/p.txt"), "grok-4.6", None, 32);
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "grok-4.6"));
        assert!(!args.iter().any(|a| a == "--reasoning-effort"));
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
        assert!(!args.iter().any(|a| a == "models"));
    }

    #[test]
    fn claude_argv_omits_model_and_effort_when_absent() {
        let args = claude_argv("", None);
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--effort"));
    }

    #[test]
    fn claude_argv_text_output_and_no_session_persistence() {
        let args = claude_argv("sonnet", Some("high"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "text")
        );
        assert!(args.iter().any(|a| a == "--no-session-persistence"));
    }

    #[test]
    fn grok_stream_reports_the_tool_and_keeps_the_reply_after_it() {
        let mut accum = GrokStreamAccum::default();
        let mut progress = Vec::new();
        let lines = [
            r#"{"type":"available_commands","tools":["run_terminal_command"]}"#,
            r#"{"type":"text","data":"I'll run it."}"#,
            r#"{"type":"tool_call","toolCallId":"c1","title":"run_terminal_command","toolName":"run_terminal_command","status":"pending","rawInput":{"command":"echo fleet-tool-marker","description":"Print the marker"}}"#,
            r#"{"type":"tool_call_update","toolCallId":"c1","status":"completed","rawOutput":{"output":[1,2,3]}}"#,
            r#"{"type":"text","data":"fleet-tool-marker"}"#,
            r#"{"type":"end","stopReason":"end_turn"}"#,
        ];
        for line in lines {
            accum.on_line(line, &mut |text| progress.push(text.to_string()));
        }
        assert_eq!(
            progress,
            vec!["Using run_terminal_command… Print the marker".to_string()],
            "tool progress must fire when the call starts, not after the CLI exits"
        );
        assert_eq!(
            accum.answer().expect("answer"),
            "fleet-tool-marker",
            "the chat reply is the text after the tool, not the preamble"
        );
    }

    #[test]
    fn grok_stream_without_tools_keeps_all_text() {
        let mut accum = GrokStreamAccum::default();
        accum.on_line(r#"{"type":"text","data":"pon"}"#, &mut |_| {});
        accum.on_line(r#"{"type":"text","data":"g"}"#, &mut |_| {});
        accum.on_line(r#"{"type":"end","stopReason":"end_turn"}"#, &mut |_| {});
        assert_eq!(accum.answer().expect("answer"), "pong");
    }

    #[test]
    fn grok_stream_empty_end_is_an_error() {
        let mut accum = GrokStreamAccum::default();
        accum.on_line(r#"{"type":"end","stopReason":"max_turns"}"#, &mut |_| {});
        let err = accum.answer().expect_err("empty");
        assert!(err.to_string().contains("max_turns"), "{err}");
    }
}
