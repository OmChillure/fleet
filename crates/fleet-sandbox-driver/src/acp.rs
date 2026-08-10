//! Minimal ACP JSON-RPC client over harness stdio.

use crate::harness::HarnessSpec;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{info, warn};

fn is_blocked_stop(stop: &str) -> bool {
    let s = stop.trim().to_ascii_lowercase();
    s == "blocked"
        || s == "denied"
        || s == "reject"
        || s == "rejected"
        || s.contains("blocked")
        || s.contains("denied")
        || s.contains("reject")
}

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
}

pub async fn run_session(
    harness: &HarnessSpec,
    work_dir: &Path,
    model: Option<&str>,
    prompt: &str,
    env_map: HashMap<String, String>,
    auto_approve: bool,
    permission_allow: &[String],
    timeout_sec: u64,
    mcp_servers: &[Value],
) -> Result<AcpOutcome> {
    let started = std::time::Instant::now();
    let mut cmd_line = vec![harness.command.clone()];
    cmd_line.extend(harness.args.iter().cloned());

    info!(
        command = ?cmd_line,
        cwd = %work_dir.display(),
        auto_approve,
        allow = ?permission_allow,
        "ACP spawn"
    );

    let mut cmd = Command::new(&harness.command);
    cmd.args(&harness.args)
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Prefer workspace toolchains via fleet-wsexec on PATH
    let path = std::env::var("PATH").unwrap_or_default();
    let path = format!("/workspace/bin:{path}");
    cmd.env("PATH", &path);
    cmd.env("FLEET_WORK_DIR", work_dir.display().to_string());
    cmd.env("FLEET_DIR", crate::provision::fleet_dir().display().to_string());
    // Harness must not inherit the volume handshake secret (same container).
    cmd.env_remove("FLEET_HANDSHAKE_SECRET");
    for (k, v) in &env_map {
        if k.starts_with("_FLEET_") || k == "FLEET_HANDSHAKE_SECRET" {
            continue;
        }
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn `{}` failed", harness.command))?;

    let stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;

    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_c = stderr_buf.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut b = stderr_c.lock().await;
            if b.len() < 40_000 {
                b.push_str(&line);
                b.push('\n');
            }
        }
    });

    let stdin = Arc::new(Mutex::new(stdin));
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));
    let transcript = Arc::new(Mutex::new(String::new()));

    let pending_r = pending.clone();
    let stdin_r = stdin.clone();
    let transcript_r = transcript.clone();
    let allow_list: Vec<String> = permission_allow.to_vec();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(id) = msg_id(&msg) {
                if msg.get("result").is_some() || msg.get("error").is_some() {
                    let mut map = pending_r.lock().await;
                    if let Some(tx) = map.remove(&id) {
                        if let Some(err) = msg.get("error") {
                            let _ = tx.send(Err(anyhow!("rpc error: {err}")));
                        } else {
                            let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                    continue;
                }
            }

            if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                if msg.get("id").is_some() {
                    handle_agent_request(&msg, method, auto_approve, &allow_list, &stdin_r).await;
                } else if method == "session/update" {
                    if let Some(update) = msg.pointer("/params/update") {
                        if let Some(piece) = format_session_update(update) {
                            // Full stream is noisy; enable with RUST_LOG=debug
                            tracing::debug!(
                                preview = %piece.chars().take(160).collect::<String>(),
                                "ACP model"
                            );
                            transcript_r.lock().await.push_str(&piece);
                        }
                    }
                }
            }
        }
    });

    let run = async {
        let init = rpc(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                    "session": { "configOptions": { "boolean": {} } }
                },
                "clientInfo": {
                    "name": "fleet-sandbox-driver",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
            &next_id,
            &pending,
            &stdin,
            30,
        )
        .await
        .context("initialize")?;

        let agent_info = init
            .get("agentInfo")
            .or_else(|| init.get("serverInfo"))
            .cloned();
        info!(?agent_info, "ACP initialize OK");

        if let Some(method_id) = pick_auth_method(&init) {
            match rpc(
                "authenticate",
                json!({ "methodId": method_id, "_meta": { "headless": true } }),
                &next_id,
                &pending,
                &stdin,
                30,
            )
            .await
            {
                Ok(_) => info!(%method_id, "ACP authenticated"),
                Err(e) => {
                    anyhow::bail!("ACP authenticate failed for {method_id}: {e}");
                }
            }
        }

        let cwd = work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf());
        let session = rpc(
            "session/new",
            json!({
                "cwd": cwd.display().to_string(),
                "mcpServers": mcp_servers,
            }),
            &next_id,
            &pending,
            &stdin,
            60,
        )
        .await
        .context("session/new")?;

        let session_id = session
            .get("sessionId")
            .or_else(|| session.get("session_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .context("session/new missing sessionId")?;
        info!(%session_id, "ACP session");

        if let Some(model) = model {
            apply_model(harness, &session_id, model, &next_id, &pending, &stdin).await;
        }

        info!(chars = prompt.len(), "ACP session/prompt");
        let prompt_result = rpc(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [ { "type": "text", "text": prompt } ]
            }),
            &next_id,
            &pending,
            &stdin,
            timeout_sec,
        )
        .await
        .context("session/prompt")?;

        let stop_reason = prompt_result
            .get("stopReason")
            .or_else(|| prompt_result.get("stop_reason"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        info!(?stop_reason, "ACP prompt complete");

        // Do not send session/cancel after prompt returns. Cancel is for aborting
        // an in-flight prompt; OpenCode (and others) answer Method not found / spam
        // stderr if we cancel a finished session. Timeout/error paths kill the child.

        let text = transcript.lock().await.clone();
        Ok::<_, anyhow::Error>((session_id, stop_reason, text, agent_info))
    };

    let outcome = match run.await {
        Ok((session_id, stop_reason, text, agent_info)) => {
            let stop_lc = stop_reason.as_deref().unwrap_or("").to_ascii_lowercase();
            let blocked = stop_reason.as_deref().is_some_and(is_blocked_stop);
            let failed_stop = stop_lc.contains("error")
                || stop_lc.contains("cancel")
                || stop_lc.contains("timeout")
                || stop_lc.contains("refusal");
            let ok = !blocked
                && !failed_stop
                && (!text.trim().is_empty()
                    || stop_reason.as_ref().is_some_and(|s| !s.trim().is_empty()));
            AcpOutcome {
                ok,
                session_id: Some(session_id),
                stop_reason,
                transcript: text,
                elapsed_sec: started.elapsed().as_secs_f64(),
                command: cmd_line,
                model: model.map(|s| s.to_string()),
                agent_info,
                error: None,
                stderr_tail: stderr_buf.lock().await.clone(),
            }
        }
        Err(e) => AcpOutcome {
            ok: false,
            session_id: None,
            stop_reason: None,
            transcript: transcript.lock().await.clone(),
            elapsed_sec: started.elapsed().as_secs_f64(),
            command: cmd_line,
            model: model.map(|s| s.to_string()),
            agent_info: None,
            error: Some(e.to_string()),
            stderr_tail: stderr_buf.lock().await.clone(),
        },
    };

    {
        let mut s = stdin.lock().await;
        let _ = s.shutdown().await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;

    Ok(outcome)
}

pub(crate) fn msg_id(msg: &Value) -> Option<u64> {
    msg.get("id").and_then(|i| {
        i.as_u64()
            .or_else(|| i.as_i64().map(|x| x as u64))
            .or_else(|| i.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Extract streamable agent text from a session update (message / thought chunks).
pub(crate) fn chunk_text(update: &Value) -> Option<String> {
    let kind = update_kind(update);
    if !matches!(
        kind.as_str(),
        "agent_message_chunk" | "agent_thought_chunk" | "message_chunk"
    ) {
        return None;
    }
    let text = update
        .pointer("/content/text")
        .and_then(|v| v.as_str())
        .or_else(|| update.get("text").and_then(|v| v.as_str()))
        .unwrap_or("");
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn update_kind(update: &Value) -> String {
    update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Format a session update for the job transcript (model text + tool activity).
pub(crate) fn format_session_update(update: &Value) -> Option<String> {
    let kind = update_kind(update);
    match kind.as_str() {
        "agent_message_chunk" | "message_chunk" => chunk_text(update),
        "agent_thought_chunk" => chunk_text(update).map(|t| format!("[thought] {t}")),
        "tool_call" | "tool_call_update" | "tool_call_start" | "tool_call_end" => {
            Some(format_tool_update(&kind, update))
        }
        "available_commands_update" | "current_mode_update" | "plan" => None,
        "" => None,
        other => {
            // Unknown update kinds: keep a short breadcrumb so we don't drop activity
            let title = update
                .get("title")
                .or_else(|| update.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if title.is_empty() {
                None
            } else {
                Some(format!("[{other}] {title}\n"))
            }
        }
    }
}

fn format_tool_update(kind: &str, update: &Value) -> String {
    let title = update
        .get("title")
        .or_else(|| update.pointer("/toolCall/title"))
        .or_else(|| update.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("tool");
    let status = update
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_id = update
        .get("toolCallId")
        .or_else(|| update.get("tool_call_id"))
        .or_else(|| update.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Common places OpenCode/Codex put the command line / args
    let detail = update
        .pointer("/rawInput")
        .or_else(|| update.pointer("/raw_input"))
        .or_else(|| update.get("content"))
        .or_else(|| update.get("locations"))
        .map(|v| {
            let s = if v.is_string() {
                v.as_str().unwrap_or("").to_string()
            } else {
                v.to_string()
            };
            s.chars().take(400).collect::<String>()
        })
        .filter(|s| !s.is_empty() && s != "null")
        .unwrap_or_default();
    let mut line = format!("[{kind}] {title}");
    if !status.is_empty() {
        line.push_str(&format!(" ({status})"));
    }
    if !tool_id.is_empty() {
        line.push_str(&format!(" id={tool_id}"));
    }
    if !detail.is_empty() {
        line.push_str(&format!(" {detail}"));
    }
    line.push('\n');
    line
}

/// Decide whether a permission request is allowed.
/// Delegates to [`crate::policy::permission_allowed`] (default-deny).
pub(crate) fn permission_allowed(
    auto_approve: bool,
    allow: &[String],
    haystack: &str,
) -> bool {
    crate::policy::permission_allowed(auto_approve, allow, haystack)
}

fn permission_haystack(msg: &Value, method: &str) -> String {
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let tool = params
        .pointer("/toolCall/title")
        .or_else(|| params.pointer("/toolCall/kind"))
        .or_else(|| params.pointer("/tool_call/title"))
        .or_else(|| params.get("toolName"))
        .or_else(|| params.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!("{method} {tool} {params}")
}

async fn handle_agent_request(
    msg: &Value,
    method: &str,
    auto_approve: bool,
    allow: &[String],
    stdin: &Mutex<ChildStdin>,
) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let result = if method.contains("permission") || method.contains("request_permission") {
        let hay = permission_haystack(msg, method);
        let allowed = permission_allowed(auto_approve, allow, &hay);
        if allowed {
            let option_id = pick_permission_option(
                msg.pointer("/params/options"),
                auto_approve,
            );
            tracing::debug!(
                %method,
                %option_id,
                hay = %hay.chars().take(80).collect::<String>(),
                "ACP permission ALLOW"
            );
            json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
        } else {
            warn!(
                %method,
                hay = %hay.chars().take(80).collect::<String>(),
                "ACP permission DENY"
            );
            json!({ "outcome": { "outcome": "cancelled" } })
        }
    } else {
        // Do not advertise unimplemented FS/terminal then auto-succeed them.
        let err = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("method not implemented: {method}")
            }
        });
        if let Ok(line) = serde_json::to_string(&err) {
            let mut s = stdin.lock().await;
            let _ = s.write_all(line.as_bytes()).await;
            let _ = s.write_all(b"\n").await;
            let _ = s.flush().await;
        }
        return;
    };
    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(line) = serde_json::to_string(&resp) {
        let mut s = stdin.lock().await;
        let _ = s.write_all(line.as_bytes()).await;
        let _ = s.write_all(b"\n").await;
        let _ = s.flush().await;
    }
}

/// Prefer once-only allow. Never select allow-always unless `auto_approve`.
pub(crate) fn pick_permission_option(options: Option<&Value>, auto_approve: bool) -> String {
    let arr = match options.and_then(|o| o.as_array()) {
        Some(a) => a,
        None => return "allow-once".into(),
    };
    let ids: Vec<(&str, String)> = arr
        .iter()
        .filter_map(|o| {
            let id = o
                .get("optionId")
                .or_else(|| o.get("option_id"))
                .or_else(|| o.get("id"))
                .and_then(|v| v.as_str())?;
            let name = o
                .get("name")
                .or_else(|| o.get("label"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            Some((id, name))
        })
        .collect();

    let is_always = |id: &str, name: &str| {
        let id_l = id.to_ascii_lowercase();
        id_l.contains("always") || name.contains("always") || id_l.contains("session")
    };
    let is_once = |id: &str, name: &str| {
        let id_l = id.to_ascii_lowercase();
        id_l.contains("once") || name.contains("once") || id_l == "allow" || id_l == "approve"
    };
    let is_allow = |id: &str, name: &str| {
        let id_l = id.to_ascii_lowercase();
        (id_l.contains("allow") || name.contains("allow") || id_l.contains("approve"))
            && !id_l.contains("deny")
            && !name.contains("deny")
    };

    if let Some((id, _)) = ids.iter().find(|(id, name)| is_once(id, name)) {
        return (*id).to_string();
    }
    if auto_approve {
        if let Some((id, _)) = ids.iter().find(|(id, name)| is_always(id, name)) {
            return (*id).to_string();
        }
    }
    if let Some((id, _)) = ids
        .iter()
        .find(|(id, name)| is_allow(id, name) && !is_always(id, name))
    {
        return (*id).to_string();
    }
    "allow-once".into()
}

pub(crate) fn pick_auth_method(init: &Value) -> Option<String> {
    let methods = init
        .get("authMethods")
        .or_else(|| init.get("auth_methods"))
        .and_then(|v| v.as_array())?;
    let ids: Vec<&str> = methods
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
        .collect();
    if ids.is_empty() {
        return None;
    }
    if std::env::var_os("XAI_API_KEY").is_some() && ids.contains(&"xai.api_key") {
        return Some("xai.api_key".into());
    }
    for preferred in ["cached_token", "api_key", "xai.api_key"] {
        if ids.contains(&preferred) {
            return Some(preferred.into());
        }
    }
    ids.into_iter()
        .find(|id| !id.to_lowercase().contains("login"))
        .map(|s| s.to_string())
}

async fn apply_model(
    harness: &HarnessSpec,
    session_id: &str,
    model: &str,
    next_id: &AtomicU64,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    stdin: &Mutex<ChildStdin>,
) {
    if harness.model_via == "none" {
        return;
    }
    let config_id = &harness.model_config_id;
    if rpc(
        "session/set_config_option",
        json!({ "sessionId": session_id, "configId": config_id, "value": model }),
        next_id,
        pending,
        stdin,
        30,
    )
    .await
    .is_ok()
    {
        info!(%model, "ACP model set via set_config_option");
        return;
    }
    match rpc(
        "session/set_model",
        json!({ "sessionId": session_id, "modelId": model }),
        next_id,
        pending,
        stdin,
        30,
    )
    .await
    {
        Ok(_) => info!(%model, "ACP model set via set_model"),
        Err(e) => warn!(error = %e, "set model failed"),
    }
}

async fn rpc(
    method: &str,
    params: Value,
    next_id: &AtomicU64,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    stdin: &Mutex<ChildStdin>,
    timeout_sec: u64,
) -> Result<Value> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    let payload = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&payload)?;
    tracing::debug!(%method, "ACP →");
    {
        let mut s = stdin.lock().await;
        s.write_all(line.as_bytes()).await?;
        s.write_all(b"\n").await?;
        s.flush().await?;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_sec), rx).await {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => bail!("agent closed while waiting for {method}"),
        Err(_) => {
            pending.lock().await.remove(&id);
            bail!("timeout waiting for {method}")
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessSpec;
    use std::path::PathBuf;

    #[test]
    fn msg_id_parses_int_and_string() {
        assert_eq!(msg_id(&json!({"id": 7})), Some(7));
        assert_eq!(msg_id(&json!({"id": "12"})), Some(12));
        assert_eq!(msg_id(&json!({})), None);
    }

    #[test]
    fn chunk_text_agent_message() {
        let u = json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" }
        });
        assert_eq!(chunk_text(&u).as_deref(), Some("hello"));
    }

    #[test]
    fn chunk_text_skips_user_and_empty() {
        assert!(chunk_text(&json!({"sessionUpdate":"user_message_chunk","content":{"text":"x"}})).is_none());
        assert!(chunk_text(&json!({"sessionUpdate":"agent_message_chunk","content":{"text":""}})).is_none());
    }

    #[test]
    fn format_session_update_includes_tool_calls() {
        let tool = json!({
            "sessionUpdate": "tool_call",
            "title": "list_dir",
            "status": "pending",
            "toolCallId": "tc1",
        });
        let s = format_session_update(&tool).unwrap();
        assert!(s.contains("list_dir"), "{s}");
        assert!(s.contains("[tool_call]"), "{s}");

        let msg = json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "working" }
        });
        assert_eq!(format_session_update(&msg).as_deref(), Some("working"));
    }

    #[test]
    fn pick_auth_prefers_cached_token() {
        let init = json!({
            "authMethods": [
                {"id": "opencode-login"},
                {"id": "cached_token"}
            ]
        });
        assert_eq!(pick_auth_method(&init).as_deref(), Some("cached_token"));
    }

    #[test]
    fn pick_auth_skips_login_only() {
        let init = json!({ "authMethods": [ {"id": "opencode-login"} ] });
        assert_eq!(pick_auth_method(&init), None);
    }

    #[tokio::test]
    async fn run_session_against_mock_agent() {
        let mock = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_acp_agent.py");
        assert!(mock.is_file(), "ACP mock fixture missing: {}", mock.display());
        which::which("python3").expect("python3 required for ACP mock session test");
        let harness = HarnessSpec {
            name: "custom".into(),
            command: "python3".into(),
            args: vec![mock.display().to_string()],
            model_via: "config_option".into(),
            model_config_id: "model".into(),
        };
        let cwd = std::env::temp_dir().join(format!("fleet-acp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let methods_log = cwd.join("methods.log");
        let mut env = HashMap::new();
        env.insert(
            "FLEET_ACP_METHODS_LOG".into(),
            methods_log.display().to_string(),
        );
        let out = run_session(
            &harness,
            &cwd,
            Some("mock-strong"),
            "Say hi",
            env,
            false,
            &["*".into()],
            30,
            &[],
        )
        .await
        .expect("acp session");
        let methods = std::fs::read_to_string(&methods_log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&cwd);
        assert!(out.ok, "stderr={}", out.stderr_tail);
        assert_eq!(out.session_id.as_deref(), Some("sess-mock-1"));
        assert!(out.transcript.to_lowercase().contains("hello") || !out.transcript.is_empty(), "transcript={}", out.transcript);
        assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
        assert!(
            !methods.lines().any(|l| l.trim() == "session/cancel"),
            "driver must not send session/cancel after prompt returns; methods={methods:?}"
        );
        assert!(
            methods.lines().any(|l| l.trim() == "session/prompt"),
            "expected session/prompt in {methods:?}"
        );
    }

    #[test]
    fn permission_default_deny() {
        assert!(!permission_allowed(false, &[], "bash npm test"));
    }

    #[test]
    fn permission_star_and_substring() {
        assert!(permission_allowed(false, &["*".into()], "anything"));
        assert!(permission_allowed(
            false,
            &["fleet-wsexec".into(), "read".into()],
            "tool fleet-wsexec django_check"
        ));
        assert!(!permission_allowed(
            false,
            &["read".into()],
            "tool fleet-wsexec django_check"
        ));
        assert!(permission_allowed(true, &[], "denied otherwise"));
    }

    #[test]
    fn pick_permission_prefers_once_never_always() {
        let opts = json!([
            {"optionId": "allow-always", "name": "Allow always"},
            {"optionId": "allow-once", "name": "Allow once"},
            {"optionId": "reject", "name": "Reject"}
        ]);
        assert_eq!(pick_permission_option(Some(&opts), false), "allow-once");
        assert_eq!(pick_permission_option(Some(&opts), true), "allow-once");
        let only_always = json!([{"optionId": "allow_always", "name": "Always"}]);
        assert_eq!(
            pick_permission_option(Some(&only_always), false),
            "allow-once"
        );
        assert_eq!(
            pick_permission_option(Some(&only_always), true),
            "allow_always"
        );
    }
}
