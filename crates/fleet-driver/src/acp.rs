//! Minimal ACP JSON-RPC client over harness stdio.
//!
//! Framing: newline-delimited JSON by default. If the first bytes look like
//! LSP `Content-Length:` headers, the reader switches to that mode (Grok Build
//! compatibility).

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::harness::HarnessSpec;
use crate::terminal::TerminalHub;

const STDERR_CAP: usize = 40_000;
const TRANSCRIPT_CAP: usize = 256_000;

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value>>>;
type SharedPending = Arc<Mutex<PendingMap>>;
type SharedStdin = Arc<Mutex<ChildStdin>>;

#[derive(Debug, Clone)]
pub struct AcpOutcome {
    pub ok: bool,
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    pub transcript: String,
    pub elapsed_sec: f64,
    pub command: Vec<String>,
    pub model: Option<String>,
    pub agent_info: Option<Value>,
    pub error: Option<String>,
    pub stderr_tail: String,
    /// Prompt-result `_meta` (tokens / model) when the agent provides it.
    pub usage: Option<Value>,
}

pub type LiveOut = Arc<std::sync::Mutex<Vec<Value>>>;

pub struct AcpRun {
    pub harness: HarnessSpec,
    pub cwd: PathBuf,
    /// Session dir for `live.jsonl` / artifacts (may differ from agent cwd after clone).
    pub session_dir: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub auto_approve: bool,
    pub timeout: Duration,
    pub extra_env: HashMap<String, String>,
    /// Plane ingest buffer. Local `live.jsonl` is debug-only.
    pub live_out: Option<LiveOut>,
}

pub async fn run_session(run: AcpRun) -> Result<AcpOutcome> {
    let started = Instant::now();
    let mut cmd_line = vec![run.harness.command.clone()];
    cmd_line.extend(run.harness.args.iter().cloned());

    info!(
        command = ?cmd_line,
        cwd = %run.cwd.display(),
        auto_approve = run.auto_approve,
        "ACP spawn"
    );

    let mut cmd = Command::new(&run.harness.command);
    cmd.args(&run.harness.args)
        .current_dir(&run.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("FLEET_WORK_DIR", run.cwd.display().to_string());
    for (k, v) in &run.extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn `{}` failed", run.harness.command))?;

    let stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;

    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_c = stderr_buf.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut b = stderr_c.lock().await;
            if b.len() < STDERR_CAP {
                b.push_str(&line);
                b.push('\n');
            }
        }
    });

    let stdin: SharedStdin = Arc::new(Mutex::new(stdin));
    let pending: SharedPending = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));
    let transcript = Arc::new(Mutex::new(String::new()));
    let cwd = run.cwd.clone();
    let auto_approve = run.auto_approve;

    let pending_r = pending.clone();
    let stdin_r = stdin.clone();
    let transcript_r = transcript.clone();
    let terminals = TerminalHub::new();
    let terminals_r = terminals.clone();
    let session_dir = run.session_dir.clone();
    let live_out = run.live_out.clone();
    let reader = tokio::spawn(async move {
        if let Err(e) = read_loop(
            stdout,
            pending_r,
            stdin_r,
            transcript_r,
            cwd,
            auto_approve,
            terminals_r,
            session_dir,
            live_out,
        )
        .await
        {
            warn!(error = %e, "ACP reader stopped");
        }
    });

    let session_fut = async {
        drive_session(
            &stdin,
            &pending,
            &next_id,
            &run.prompt,
            run.model.as_deref(),
            &run.cwd,
        )
        .await
    };

    let timed = tokio::time::timeout(run.timeout, session_fut).await;

    let mut outcome = match timed {
        Ok(Ok(partial)) => partial,
        Ok(Err(e)) => AcpPartial {
            ok: false,
            session_id: None,
            stop_reason: None,
            model: run.model.clone(),
            agent_info: None,
            error: Some(format!("{e:#}")),
            usage: None,
        },
        Err(_) => {
            let _ = rpc(
                &stdin,
                &pending,
                &next_id,
                "session/cancel",
                json!({}),
                Duration::from_secs(2),
            )
            .await;
            AcpPartial {
                ok: false,
                session_id: None,
                stop_reason: Some("timeout".into()),
                model: run.model.clone(),
                agent_info: None,
                error: Some(format!("ACP session timed out after {:?}", run.timeout)),
                usage: None,
            }
        }
    };

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    reader.abort();

    let transcript_text = {
        let t = transcript.lock().await;
        truncate_chars(&t, TRANSCRIPT_CAP)
    };
    let stderr_tail = stderr_buf.lock().await.clone();

    if outcome.ok && transcript_text.trim().is_empty() {
        let sr = outcome.stop_reason.as_deref().unwrap_or("");
        if !sr.eq_ignore_ascii_case("end_turn") {
            outcome.ok = false;
            if outcome.error.is_none() {
                outcome.error = Some("ACP completed with empty transcript".into());
            }
        }
    }

    Ok(AcpOutcome {
        ok: outcome.ok,
        session_id: outcome.session_id,
        stop_reason: outcome.stop_reason,
        transcript: transcript_text,
        elapsed_sec: started.elapsed().as_secs_f64(),
        command: cmd_line,
        model: outcome.model.or(run.model),
        agent_info: outcome.agent_info,
        error: outcome.error,
        stderr_tail,
        usage: outcome.usage,
    })
}

struct AcpPartial {
    ok: bool,
    session_id: Option<String>,
    stop_reason: Option<String>,
    model: Option<String>,
    agent_info: Option<Value>,
    error: Option<String>,
    usage: Option<Value>,
}

async fn drive_session(
    stdin: &SharedStdin,
    pending: &SharedPending,
    next_id: &Arc<AtomicU64>,
    prompt: &str,
    model: Option<&str>,
    cwd: &Path,
) -> Result<AcpPartial> {
    let init = rpc(
        stdin,
        pending,
        next_id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true
            },
            "clientInfo": {
                "name": "fleet-driver",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
        Duration::from_secs(30),
    )
    .await
    .context("initialize")?;

    let agent_info = init
        .get("agentInfo")
        .cloned()
        .or_else(|| init.get("agent_info").cloned());
    info!(?agent_info, "ACP initialize OK");

    if let Some(method_id) = pick_auth_method(&init) {
        match rpc(
            stdin,
            pending,
            next_id,
            "authenticate",
            json!({ "methodId": method_id, "_meta": { "headless": true } }),
            Duration::from_secs(30),
        )
        .await
        {
            Ok(_) => info!(%method_id, "ACP authenticated"),
            Err(e) => {
                anyhow::bail!("ACP authenticate failed for {method_id}: {e}");
            }
        }
    }

    let new_sess = rpc(
        stdin,
        pending,
        next_id,
        "session/new",
        json!({
            "cwd": cwd.display().to_string(),
            "mcpServers": []
        }),
        Duration::from_secs(30),
    )
    .await
    .context("session/new")?;

    let session_id = new_sess
        .get("sessionId")
        .or_else(|| new_sess.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("session/new missing sessionId")?;

    let session_model = new_sess
        .pointer("/models/currentModelId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(m) = model
        && let Some(opt_id) = pick_model_option(&new_sess)
    {
        let _ = rpc(
            stdin,
            pending,
            next_id,
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": opt_id,
                "value": m,
            }),
            Duration::from_secs(15),
        )
        .await;
    }

    info!(chars = prompt.len(), "ACP session/prompt");
    let prompt_result = rpc(
        stdin,
        pending,
        next_id,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": prompt }],
        }),
        Duration::from_secs(24 * 3600), // outer timeout wraps this
    )
    .await
    .context("session/prompt")?;

    let stop_reason = prompt_result
        .get("stopReason")
        .or_else(|| prompt_result.get("stop_reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let usage = prompt_result.get("_meta").cloned();
    let usage_model = usage
        .as_ref()
        .and_then(|u| u.get("modelId").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let ok = session_succeeded(stop_reason.as_deref());

    let _ = rpc(
        stdin,
        pending,
        next_id,
        "session/close",
        json!({ "sessionId": session_id }),
        Duration::from_secs(5),
    )
    .await;

    Ok(AcpPartial {
        ok,
        session_id: Some(session_id),
        stop_reason: stop_reason.clone(),
        model: model
            .map(|s| s.to_string())
            .or(usage_model)
            .or(session_model),
        agent_info,
        error: if ok {
            None
        } else {
            Some(format!(
                "stop_reason={}",
                stop_reason.as_deref().unwrap_or("unknown")
            ))
        },
        usage,
    })
}

async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: SharedPending,
    stdin: SharedStdin,
    transcript: Arc<Mutex<String>>,
    cwd: PathBuf,
    auto_approve: bool,
    terminals: TerminalHub,
    session_dir: PathBuf,
    live_out: Option<LiveOut>,
) -> Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut mode = FrameMode::Auto;
    loop {
        let Some(msg) = read_message(&mut reader, &mut mode).await? else {
            return Ok(());
        };

        if let Some(id) = msg_id(&msg)
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&id) {
                if let Some(err) = msg.get("error") {
                    let _ = tx.send(Err(anyhow!("rpc error: {err}")));
                } else {
                    let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
            continue;
        }

        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if msg.get("id").is_some() {
                handle_agent_request(&msg, method, auto_approve, &stdin, &cwd, &terminals).await;
            } else if method == "session/update"
                && let Some(update) = msg
                    .pointer("/params/update")
                    .or_else(|| msg.pointer("/params"))
                && let Some(piece) = format_session_update(update)
            {
                debug!(
                    preview = %piece.chars().take(160).collect::<String>(),
                    "ACP update"
                );
                append_live_event(&session_dir, live_out.as_ref(), "update", &piece);
                let mut t = transcript.lock().await;
                if t.len() < TRANSCRIPT_CAP {
                    t.push_str(&piece);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameMode {
    Auto,
    Ndjson,
    Lsp,
}

async fn read_message(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    mode: &mut FrameMode,
) -> Result<Option<Value>> {
    match *mode {
        FrameMode::Ndjson => read_ndjson(reader).await,
        FrameMode::Lsp => read_lsp(reader).await,
        FrameMode::Auto => {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            // Skip leading whitespace / BOMs-ish.
            let peek = std::str::from_utf8(buf).unwrap_or("");
            let trimmed = peek.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
            if trimmed.starts_with("Content-Length:") || trimmed.starts_with("content-length:") {
                *mode = FrameMode::Lsp;
                info!("ACP framing: Content-Length");
                read_lsp(reader).await
            } else {
                *mode = FrameMode::Ndjson;
                read_ndjson(reader).await
            }
        }
    }
}

async fn read_ndjson(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Option<Value>> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(t) {
            Ok(v) => return Ok(Some(v)),
            Err(_) => {
                debug!(preview = %t.chars().take(80).collect::<String>(), "skip non-JSON stdout");
                continue;
            }
        }
    }
}

async fn read_lsp(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Option<Value>> {
    let mut content_len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            content_len = v.trim().parse().ok();
        }
    }
    let Some(len) = content_len else {
        bail!("LSP frame missing Content-Length");
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let v = serde_json::from_slice(&buf).context("LSP JSON body")?;
    Ok(Some(v))
}

async fn rpc(
    stdin: &SharedStdin,
    pending: &SharedPending,
    next_id: &Arc<AtomicU64>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    debug!(%method, id, "ACP →");
    {
        let mut w = stdin.lock().await;
        let line = serde_json::to_string(&msg)?;
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => bail!("agent closed while waiting for {method}"),
        Err(_) => {
            pending.lock().await.remove(&id);
            bail!("timeout waiting for {method}")
        }
    }
}

async fn handle_agent_request(
    msg: &Value,
    method: &str,
    auto_approve: bool,
    stdin: &SharedStdin,
    cwd: &Path,
    terminals: &TerminalHub,
) {
    let Some(id) = msg.get("id").cloned() else {
        return;
    };
    let result = if method_is_permission(method) {
        if auto_approve {
            info!(%method, "ACP permission ALLOW");
            permission_allow_result(msg)
        } else {
            info!(%method, "ACP permission DENY");
            json!({
                "outcome": { "outcome": "cancelled" }
            })
        }
    } else if method_is_read_file(method) {
        match read_text_file(cwd, msg) {
            Ok(text) => json!({ "content": text }),
            Err(e) => {
                send_error(stdin, id, e.to_string()).await;
                return;
            }
        }
    } else if method_is_write_file(method) {
        match write_text_file(cwd, msg) {
            Ok(()) => json!({}),
            Err(e) => {
                send_error(stdin, id, e.to_string()).await;
                return;
            }
        }
    } else if method_is_terminal(method) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match terminals.handle(method, &params, cwd).await {
            Ok(v) => v,
            Err(e) => {
                warn!(%method, error = %e, "ACP terminal method failed");
                send_error(stdin, id, e.to_string()).await;
                return;
            }
        }
    } else {
        warn!(%method, "ACP agent request not implemented");
        send_error(stdin, id, format!("method not implemented: {method}")).await;
        return;
    };

    let reply = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    let mut w = stdin.lock().await;
    if let Ok(line) = serde_json::to_string(&reply) {
        let _ = w.write_all(line.as_bytes()).await;
        let _ = w.write_all(b"\n").await;
        let _ = w.flush().await;
    }
}

async fn send_error(stdin: &SharedStdin, id: Value, message: String) {
    let reply = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": message },
    });
    let mut w = stdin.lock().await;
    if let Ok(line) = serde_json::to_string(&reply) {
        let _ = w.write_all(line.as_bytes()).await;
        let _ = w.write_all(b"\n").await;
        let _ = w.flush().await;
    }
}

fn method_is_permission(method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    m.contains("permission") || m.contains("request_permission")
}

fn method_is_read_file(method: &str) -> bool {
    let m = method.to_ascii_lowercase().replace('-', "_");
    m.contains("read_text_file") || m.contains("readtextfile")
}

fn method_is_write_file(method: &str) -> bool {
    let m = method.to_ascii_lowercase().replace('-', "_");
    m.contains("write_text_file") || m.contains("writetextfile")
}

pub const LIVE_BUF_CAP: usize = 64;

fn append_live_event(session_dir: &Path, live_out: Option<&LiveOut>, kind: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let rec = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "kind": kind,
        "text": text,
    });
    let path = session_dir.join("live.jsonl");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path)
        && let Ok(line) = serde_json::to_string(&rec)
    {
        let _ = writeln!(f, "{line}");
    }
    if let Some(buf) = live_out
        && let Ok(mut g) = buf.lock()
    {
        if g.len() >= LIVE_BUF_CAP {
            let overflow = g.len() + 1 - LIVE_BUF_CAP;
            g.drain(0..overflow);
        }
        g.push(rec);
    }
}

fn method_is_terminal(method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    m.starts_with("terminal/") || m.contains("terminal/create") || m.contains("create_terminal")
}

fn permission_allow_result(msg: &Value) -> Value {
    let options = msg
        .pointer("/params/options")
        .or_else(|| msg.pointer("/params/permissionOptions"))
        .and_then(|v| v.as_array());
    let option_id = options
        .and_then(|opts| {
            opts.iter().find_map(|o| {
                let id = o.get("optionId").or_else(|| o.get("option_id"))?.as_str()?;
                let lower = id.to_ascii_lowercase();
                if lower.contains("allow") {
                    Some(id.to_string())
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            options.and_then(|opts| {
                opts.first()
                    .and_then(|o| o.get("optionId").or_else(|| o.get("option_id")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        })
        .unwrap_or_else(|| "allow-once".into());
    json!({
        "outcome": {
            "outcome": "selected",
            "optionId": option_id
        }
    })
}

fn read_text_file(cwd: &Path, msg: &Value) -> Result<String> {
    let path = msg
        .pointer("/params/path")
        .or_else(|| msg.pointer("/params/uri"))
        .and_then(|v| v.as_str())
        .context("read file missing path")?;
    let resolved = resolve_under_cwd(cwd, path)?;
    std::fs::read_to_string(&resolved).with_context(|| format!("read {}", resolved.display()))
}

fn write_text_file(cwd: &Path, msg: &Value) -> Result<()> {
    let path = msg
        .pointer("/params/path")
        .or_else(|| msg.pointer("/params/uri"))
        .and_then(|v| v.as_str())
        .context("write file missing path")?;
    let content = msg
        .pointer("/params/content")
        .or_else(|| msg.pointer("/params/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let resolved = resolve_under_cwd(cwd, path)?;
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&resolved, content).with_context(|| format!("write {}", resolved.display()))
}

fn resolve_under_cwd(cwd: &Path, path: &str) -> Result<PathBuf> {
    let raw = path.strip_prefix("file://").unwrap_or(path);
    let p = PathBuf::from(raw);
    for c in p.components() {
        if matches!(c, std::path::Component::ParentDir) {
            bail!("path escapes work dir: {path}");
        }
    }
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if abs.exists() {
        let canon = abs.canonicalize().unwrap_or(abs);
        if !canon.starts_with(&canon_cwd) {
            bail!("path escapes work dir: {path}");
        }
        return Ok(canon);
    }
    let parent = abs.parent().unwrap_or(cwd);
    let canon_parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if !canon_parent.starts_with(&canon_cwd) {
        bail!("path escapes work dir: {path}");
    }
    Ok(abs)
}

fn format_session_update(update: &Value) -> Option<String> {
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" | "message_chunk" => {
            let text = update
                .pointer("/content/text")
                .or_else(|| update.pointer("/text"))
                .and_then(|v| v.as_str())?;
            if text.is_empty() {
                return None;
            }
            if kind.contains("thought") {
                Some(format!("_thought:_ {text}\n"))
            } else {
                Some(text.to_string())
            }
        }
        "tool_call" | "tool_call_update" => {
            let title = update
                .get("title")
                .or_else(|| update.get("toolName"))
                .or_else(|| update.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let extra = update
                .pointer("/content/0/content/text")
                .or_else(|| update.pointer("/content/0/text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut line = format!("\n[{kind}: {title}");
            if !status.is_empty() {
                line.push_str(" ");
                line.push_str(status);
            }
            line.push_str("]\n");
            if !extra.is_empty() {
                line.push_str(extra);
                line.push('\n');
            }
            Some(line)
        }
        _ => {
            // Sometimes the update *is* the content object.
            if let Some(text) = update.pointer("/content/text").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                return Some(text.to_string());
            }
            None
        }
    }
}

fn pick_auth_method(init: &Value) -> Option<String> {
    let methods = init
        .get("authMethods")
        .or_else(|| init.get("auth_methods"))?
        .as_array()?;
    let ids: Vec<&str> = methods
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .collect();
    if ids.is_empty() {
        return None;
    }
    ids.iter()
        .copied()
        .find(|id| id.contains("cached") || id.contains("token") || *id == "none")
        .or_else(|| ids.first().copied())
        .map(|s| s.to_string())
}

fn pick_model_option(session_new: &Value) -> Option<String> {
    let opts = session_new.get("configOptions")?.as_array()?;
    opts.iter().find_map(|o| {
        let id = o.get("id")?.as_str()?;
        let cat = o.get("category").and_then(|v| v.as_str()).unwrap_or("");
        if id == "model" || cat == "model" {
            Some(id.to_string())
        } else {
            None
        }
    })
}

fn msg_id(msg: &Value) -> Option<u64> {
    match msg.get("id")? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// ACP `stopReason` that means the agent refused / was policy-blocked.
pub fn is_blocked_stop(stop: &str) -> bool {
    let s = stop.trim().to_ascii_lowercase();
    s == "blocked"
        || s == "denied"
        || s == "reject"
        || s == "rejected"
        || s.contains("blocked")
        || s.contains("denied")
        || s.contains("reject")
}

/// Whether a completed prompt counts as success.
///
/// Unknown reasons stay successful (agent finished). Block/deny/reject and the
/// known failure tokens do not — even when the transcript is non-empty.
pub fn session_succeeded(stop_reason: Option<&str>) -> bool {
    let Some(raw) = stop_reason else {
        return true;
    };
    let s = raw.trim();
    if s.is_empty() {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "end_turn" | "done" => true,
        "cancelled" | "canceled" | "timeout" | "refusal" | "error" => false,
        other if is_blocked_stop(other) => false,
        _ => true,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_cached_token() {
        let init = json!({
            "authMethods": [
                {"id": "oauth", "name": "OAuth"},
                {"id": "cached_token", "name": "Cached"}
            ]
        });
        assert_eq!(pick_auth_method(&init).as_deref(), Some("cached_token"));
    }

    #[test]
    fn pick_auth_none() {
        let init = json!({});
        assert_eq!(pick_auth_method(&init), None);
    }

    #[test]
    fn format_message_chunk() {
        let u = json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hello"}
        });
        assert_eq!(format_session_update(&u).as_deref(), Some("hello"));
    }

    #[test]
    fn blocked_stop_is_not_success() {
        assert!(!session_succeeded(Some("blocked")));
        assert!(!session_succeeded(Some("Blocked")));
        assert!(!session_succeeded(Some("permission_denied")));
        assert!(!session_succeeded(Some("rejected")));
        assert!(is_blocked_stop("blocked"));
        assert!(is_blocked_stop("policy-blocked"));
    }

    #[test]
    fn session_succeeded_none_empty_end_turn_done_and_unknown() {
        assert!(session_succeeded(None));
        assert!(session_succeeded(Some("")));
        assert!(session_succeeded(Some("end_turn")));
        assert!(session_succeeded(Some("done")));
        assert!(session_succeeded(Some("max_tokens")));
        assert!(!is_blocked_stop("end_turn"));
    }

    #[test]
    fn session_succeeded_false_for_cancelled_timeout_error_refusal() {
        assert!(!session_succeeded(Some("cancelled")));
        assert!(!session_succeeded(Some("timeout")));
        assert!(!session_succeeded(Some("error")));
        assert!(!session_succeeded(Some("refusal")));
    }

    #[test]
    fn refusal_stop_is_failed_not_blocked() {
        assert!(!session_succeeded(Some("refusal")));
        assert!(!is_blocked_stop("refusal"));
    }

    #[test]
    fn session_succeeded_canceled_us_spelling_is_fail() {
        assert!(!session_succeeded(Some("canceled")));
    }

    #[test]
    fn session_succeeded_trims_and_lowercases_fail_tokens() {
        assert!(!session_succeeded(Some(" Cancelled ")));
        assert!(!session_succeeded(Some("TIMEOUT")));
    }

    #[test]
    fn session_succeeded_unknown_error_substring_stays_success() {
        assert!(session_succeeded(Some("session_error")));
    }

    #[test]
    fn permission_allow_result_selects_allow_once() {
        let msg = json!({
            "params": {
                "options": [
                    { "optionId": "allow-once" },
                    { "optionId": "reject" }
                ]
            }
        });
        let got = permission_allow_result(&msg);
        assert_eq!(
            got.pointer("/outcome/outcome").and_then(|v| v.as_str()),
            Some("selected")
        );
        assert_eq!(
            got.pointer("/outcome/optionId").and_then(|v| v.as_str()),
            Some("allow-once")
        );
    }

    #[test]
    fn permission_allow_result_skips_reject_for_allow() {
        let msg = json!({
            "params": {
                "options": [
                    { "optionId": "reject" },
                    { "optionId": "allow-once" }
                ]
            }
        });
        let got = permission_allow_result(&msg);
        assert_eq!(
            got.pointer("/outcome/optionId").and_then(|v| v.as_str()),
            Some("allow-once")
        );
    }
}
