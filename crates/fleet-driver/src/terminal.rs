//! ACP client terminal methods (`terminal/create` … `terminal/release`).
//!
//! Grok (and other ACP agents) call these on the *client* when we advertise
//! `clientCapabilities.terminal = true`. Returning `{}` makes the agent fail
//! with `failed to deserialize response` before any command starts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct TerminalHub {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<Term>>>>>,
    seq: Arc<AtomicU64>,
}

struct Term {
    child: Option<Child>,
    output: String,
    truncated: bool,
    limit: usize,
    exit_code: Option<i32>,
    signal: Option<String>,
    done: Arc<AtomicBool>,
}

impl TerminalHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn handle(&self, method: &str, params: &Value, session_cwd: &Path) -> Result<Value> {
        let m = method.trim_start_matches("terminal/");
        match m {
            "create" => self.create(params, session_cwd).await,
            "output" => self.output(params).await,
            "wait_for_exit" => self.wait_for_exit(params).await,
            "kill" => self.kill(params).await,
            "release" => self.release(params).await,
            other => bail!("unknown terminal method {other}"),
        }
    }

    async fn create(&self, params: &Value, session_cwd: &Path) -> Result<Value> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .context("terminal/create missing command")?;
        let args: Vec<String> = params
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| session_cwd.to_path_buf());
        let cwd = resolve_cwd(session_cwd, &cwd)?;
        let limit = params
            .get("outputByteLimit")
            .and_then(|v| v.as_u64())
            .unwrap_or(1_048_576) as usize;

        let mut cmd = if args.is_empty() && looks_like_shell_line(command) {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        } else {
            let mut c = Command::new(command);
            c.args(&args);
            c
        };
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(env) = params.get("env").and_then(|v| v.as_array()) {
            for item in env {
                let name = item.get("name").and_then(|v| v.as_str());
                let value = item.get("value").and_then(|v| v.as_str());
                if let (Some(n), Some(val)) = (name, value) {
                    cmd.env(n, val);
                }
            }
        }

        info!(command, ?args, cwd = %cwd.display(), "ACP terminal/create");
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn terminal command `{command}`"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let id = format!("term-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let done = Arc::new(AtomicBool::new(false));
        let term = Arc::new(Mutex::new(Term {
            child: Some(child),
            output: String::new(),
            truncated: false,
            limit,
            exit_code: None,
            signal: None,
            done: done.clone(),
        }));

        if let Some(out) = stdout {
            spawn_pump(term.clone(), out, false);
        }
        if let Some(err) = stderr {
            spawn_pump(term.clone(), err, true);
        }

        self.inner.lock().await.insert(id.clone(), term);
        Ok(json!({ "terminalId": id }))
    }

    async fn get(&self, params: &Value) -> Result<Arc<Mutex<Term>>> {
        let id = params
            .get("terminalId")
            .and_then(|v| v.as_str())
            .context("missing terminalId")?;
        self.inner
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown terminalId {id}"))
    }

    async fn output(&self, params: &Value) -> Result<Value> {
        let term = self.get(params).await?;
        let t = term.lock().await;
        Ok(json!({
            "output": t.output,
            "truncated": t.truncated,
            "exitStatus": t.exit_status_json(),
        }))
    }

    async fn wait_for_exit(&self, params: &Value) -> Result<Value> {
        let term = self.get(params).await?;
        loop {
            {
                let mut t = term.lock().await;
                if t.exit_code.is_some() || t.signal.is_some() {
                    return Ok(json!({
                        "exitCode": t.exit_code,
                        "signal": t.signal,
                    }));
                }
                if let Some(child) = t.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            apply_exit(&mut t, status);
                            return Ok(json!({
                                "exitCode": t.exit_code,
                                "signal": t.signal,
                            }));
                        }
                        Ok(None) => {}
                        Err(e) => bail!("wait terminal: {e}"),
                    }
                } else if t.done.load(Ordering::Relaxed) {
                    return Ok(json!({
                        "exitCode": t.exit_code,
                        "signal": t.signal,
                    }));
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
    }

    async fn kill(&self, params: &Value) -> Result<Value> {
        let term = self.get(params).await?;
        let mut t = term.lock().await;
        if let Some(child) = t.child.as_mut() {
            let _ = child.start_kill();
        }
        Ok(json!({}))
    }

    async fn release(&self, params: &Value) -> Result<Value> {
        let id = params
            .get("terminalId")
            .and_then(|v| v.as_str())
            .context("missing terminalId")?;
        if let Some(term) = self.inner.lock().await.remove(id) {
            let mut t = term.lock().await;
            if let Some(mut child) = t.child.take() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
        Ok(json!({}))
    }
}

impl Term {
    fn exit_status_json(&self) -> Value {
        if self.exit_code.is_none() && self.signal.is_none() {
            return Value::Null;
        }
        json!({
            "exitCode": self.exit_code,
            "signal": self.signal,
        })
    }
}

fn apply_exit(t: &mut Term, status: std::process::ExitStatus) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        t.exit_code = status.code();
        t.signal = status.signal().map(|s| s.to_string());
    }
    #[cfg(not(unix))]
    {
        t.exit_code = status.code();
        t.signal = None;
    }
    t.done.store(true, Ordering::Relaxed);
}

fn spawn_pump<R>(term: Arc<Mutex<Term>>, reader: R, is_stderr: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let mut r = BufReader::new(reader);
        loop {
            match r.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    let mut t = term.lock().await;
                    let limit = t.limit;
                    t.output.push_str(&chunk);
                    if t.output.len() > limit {
                        t.truncated = true;
                        let drain = t.output.len() - limit;
                        let start = t
                            .output
                            .char_indices()
                            .find(|(i, _)| *i >= drain)
                            .map(|(i, _)| i)
                            .unwrap_or(drain);
                        t.output.drain(..start);
                    }
                    let _ = is_stderr;
                }
                Err(e) => {
                    warn!(error = %e, "terminal pump read error");
                    break;
                }
            }
        }
    });
}

fn looks_like_shell_line(cmd: &str) -> bool {
    cmd.contains(' ')
        || cmd.contains('|')
        || cmd.contains('&')
        || cmd.contains(';')
        || cmd.contains('>')
        || cmd.contains('<')
        || cmd.contains('$')
        || cmd.contains('`')
}

fn resolve_cwd(session_cwd: &Path, requested: &Path) -> Result<PathBuf> {
    let abs = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        session_cwd.join(requested)
    };
    let canon_session = session_cwd
        .canonicalize()
        .unwrap_or_else(|_| session_cwd.to_path_buf());
    let canon = abs.canonicalize().unwrap_or(abs);
    if !canon.starts_with(&canon_session) {
        bail!("terminal cwd escapes session work dir");
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_line_detect() {
        assert!(looks_like_shell_line("echo hello && cargo fmt"));
        assert!(!looks_like_shell_line("cargo"));
    }
}
