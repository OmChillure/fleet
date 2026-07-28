//! Minimal ACP (Agent Client Protocol) JSON-RPC client over stdio.

use crate::models::{ConfigChoice, DiscoveredConfig, McpServerConfig, SkillRef};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

/// How log text should be rendered in the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPiece {
    /// Streaming token/text — append onto the current stream line.
    Stream { stream: StreamKind, text: String },
    /// Discrete event — always starts a new log line.
    Line(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Agent,
    Thought,
    User,
}

pub type LogFn = Arc<dyn Fn(LogPiece) + Send + Sync>;

pub struct AcpRunRequest {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub thought_level: Option<String>,
    pub system_prompt: String,
    pub user_prompt: String,
    pub skills: Vec<SkillRef>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub auto_approve: bool,
    pub log: LogFn,
}

#[derive(Debug)]
pub struct AcpRunResult {
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    pub transcript: String,
}

pub fn build_mcp_params(servers: &[McpServerConfig]) -> Value {
    let list: Vec<Value> = servers
        .iter()
        .map(|s| {
            if s.r#type.as_deref() == Some("http") || s.url.is_some() {
                let headers: Vec<Value> = s
                    .headers
                    .iter()
                    .map(|(k, v)| json!({"name": k, "value": v}))
                    .collect();
                json!({
                    "type": "http",
                    "name": s.name,
                    "url": s.url,
                    "headers": headers,
                })
            } else {
                let env: Vec<Value> = s
                    .env
                    .iter()
                    .map(|(k, v)| json!({"name": k, "value": v}))
                    .collect();
                json!({
                    "name": s.name,
                    "command": s.command,
                    "args": s.args,
                    "env": env,
                })
            }
        })
        .collect();
    Value::Array(list)
}

/// Combine task title + body so the agent sees the full request.
///
/// Users often put the actual ask in the title and persona/context in the prompt
/// (or the reverse). Sending only `prompt` drops the title and confuses the model.
pub fn compose_user_message(title: &str, prompt: &str) -> String {
    let title = title.trim();
    let prompt = prompt.trim();
    match (title.is_empty(), prompt.is_empty()) {
        (true, true) => String::new(),
        (false, true) => title.to_string(),
        (true, false) => prompt.to_string(),
        (false, false) => {
            // Avoid duplicating if the body already starts with / contains the title.
            if prompt == title || prompt.starts_with(title) || prompt.contains(title) {
                prompt.to_string()
            } else {
                format!("{title}\n\n{prompt}")
            }
        }
    }
}

pub fn compose_prompt(system: &str, skills: &[SkillRef], user: &str, cwd: &Path) -> String {
    let mut parts = Vec::new();
    if !system.trim().is_empty() {
        parts.push(system.trim().to_string());
    }

    let enabled: Vec<&SkillRef> = skills.iter().filter(|s| s.enabled).collect();
    if !enabled.is_empty() {
        parts.push("## Skills".into());
        for sk in enabled {
            let path = PathBuf::from(&sk.path);
            let resolved = if path.is_absolute() {
                path
            } else {
                cwd.join(&sk.path)
            };
            if resolved.is_file() {
                match std::fs::read_to_string(&resolved) {
                    Ok(body) => parts.push(format!("### Skill `{}`\n{body}", sk.path)),
                    Err(_) => parts.push(format!("(skill file unreadable: {})", resolved.display())),
                }
            } else if resolved.is_dir() {
                let skill_md = resolved.join("SKILL.md");
                if skill_md.is_file() {
                    match std::fs::read_to_string(&skill_md) {
                        Ok(body) => parts.push(format!("### Skill `{}`\n{body}", sk.path)),
                        Err(_) => parts.push(format!("(skill unreadable: {})", skill_md.display())),
                    }
                } else {
                    parts.push(format!(
                        "(skill dir without SKILL.md: {})",
                        resolved.display()
                    ));
                }
            } else {
                parts.push(format!("(skill not found: {})", sk.path));
            }
        }
    }

    parts.push("## Task".into());
    parts.push(user.trim().to_string());
    parts.join("\n\n")
}

pub(crate) fn format_update(update: &Value) -> Option<LogPiece> {
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match kind {
        // Echo of our own prompt — we already log outbound text; skip to avoid
        // gluing the prompt into the assistant stream in the run log.
        "user_message_chunk" => None,
        "agent_message_chunk" | "agent_thought_chunk" => {
            let text = update
                .pointer("/content/text")
                .and_then(|v| v.as_str())
                .or_else(|| update.get("text").and_then(|v| v.as_str()))
                .unwrap_or("");
            if text.is_empty() {
                return None;
            }
            let stream = if kind.contains("thought") {
                StreamKind::Thought
            } else {
                StreamKind::Agent
            };
            Some(LogPiece::Stream {
                stream,
                text: text.to_string(),
            })
        }
        "tool_call" => {
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .or_else(|| update.get("kind").and_then(|v| v.as_str()))
                .or_else(|| {
                    update
                        .pointer("/content/text")
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("tool");
            let tool_id = update
                .get("toolCallId")
                .or_else(|| update.get("tool_call_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let id_bit = if tool_id.is_empty() {
                String::new()
            } else {
                format!(" ({tool_id})")
            };
            Some(LogPiece::Line(format!("🔧 tool_call{id_bit}: {title}")))
        }
        "tool_call_update" => {
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("update");
            let tool_id = update
                .get("toolCallId")
                .or_else(|| update.get("tool_call_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Prefer content text (MCP/tool result snippet) when present
            let detail = update
                .pointer("/content/0/text")
                .or_else(|| update.pointer("/content/text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let detail = if detail.is_empty() {
                String::new()
            } else {
                let short: String = detail.chars().take(200).collect();
                let more = if detail.chars().count() > 200 { "…" } else { "" };
                format!(" — {short}{more}")
            };
            let id_bit = if tool_id.is_empty() {
                String::new()
            } else {
                format!(" {tool_id}")
            };
            Some(LogPiece::Line(format!(
                "🔧 tool_update{id_bit}: {status}{detail}"
            )))
        }
        // High-frequency Grok noise — collapse to nothing in the run log
        "available_commands_update" => None,
        "config_option_update" => Some(LogPiece::Line("⚙️  config updated".into())),
        "current_mode_update" | "mode_update" => {
            let mode = update
                .get("currentModeId")
                .or_else(|| update.get("modeId"))
                .or_else(|| update.get("mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            Some(LogPiece::Line(format!("⚙️  mode → {mode}")))
        }
        other => {
            if other.is_empty()
                || other == "session_info_update"
                || other == "usage_update"
            {
                // Protocol bookkeeping — don't pollute the human log
                None
            } else {
                Some(LogPiece::Line(format!("… {other}")))
            }
        }
    }
}

/// Builds a human-readable run transcript with clear stream boundaries.
/// (Previously we concatenated all chunks → prompt/thought/tools looked "mixed".)
#[derive(Default)]
struct TranscriptBuilder {
    buf: String,
    active: Option<StreamKind>,
}

impl TranscriptBuilder {
    fn push_stream(&mut self, stream: StreamKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.active != Some(stream) {
            if !self.buf.is_empty() && !self.buf.ends_with('\n') {
                self.buf.push('\n');
            }
            if !self.buf.is_empty() {
                self.buf.push('\n');
            }
            let header = match stream {
                StreamKind::User => "── user ──\n",
                StreamKind::Thought => "── thought ──\n",
                StreamKind::Agent => "── assistant ──\n",
            };
            self.buf.push_str(header);
            self.active = Some(stream);
        }
        self.buf.push_str(text);
    }

    fn push_line(&mut self, line: &str) {
        self.active = None;
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
        self.buf.push_str(line);
        self.buf.push('\n');
    }

    fn finish(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }
}

async fn handle_agent_request(
    msg: &Value,
    method: &str,
    auto_approve: bool,
    stdin: &Arc<Mutex<ChildStdin>>,
    log: &LogFn,
) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    log(LogPiece::Line(format!("← request {method}")));

    let result = if method.contains("request_permission") {
        if auto_approve {
            let option_id = msg
                .pointer("/params/options/0/optionId")
                .or_else(|| msg.pointer("/params/options/0/option_id"))
                .or_else(|| msg.pointer("/params/options/0/id"))
                .and_then(|v| v.as_str())
                .unwrap_or("allow-once");
            json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id
                }
            })
        } else {
            json!({ "outcome": { "outcome": "cancelled" } })
        }
    } else {
        json!({})
    };

    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(line) = serde_json::to_string(&resp) {
        let mut s = stdin.lock().await;
        let _ = s.write_all(line.as_bytes()).await;
        let _ = s.write_all(b"\n").await;
        let _ = s.flush().await;
    }
}

async fn rpc(
    method: &str,
    params: Value,
    next_id: &AtomicU64,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    stdin: &Mutex<ChildStdin>,
    log: &LogFn,
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
    log(LogPiece::Line(format!("→ {method}")));
    {
        let mut s = stdin.lock().await;
        s.write_all(line.as_bytes()).await?;
        s.write_all(b"\n").await?;
        s.flush().await?;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(600), rx).await {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => Err(anyhow!("agent closed while waiting for {method}")),
        Err(_) => Err(anyhow!("timeout waiting for {method}")),
    }
}


/// Choose an ACP auth method from initialize result (Grok uses xai.api_key / cached_token).
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
    if std::env::var_os("XAI_API_KEY").is_some() && ids.iter().any(|id| *id == "xai.api_key") {
        return Some("xai.api_key".into());
    }
    if ids.iter().any(|id| *id == "cached_token") {
        return Some("cached_token".into());
    }
    // Fall back to first advertised method
    Some(ids[0].to_string())
}


/// Parse model / thought / mode options from a `session/new` response.
///
/// Agents expose this in different shapes:
/// - **Standard ACP**: `configOptions[]` with `category: "model" | "thought_level" | "mode"`
/// - **Grok Build**: `models.availableModels[]` (`modelId`) + reasoning efforts in
///   `model._meta.reasoningEfforts` and/or `_meta["x.ai/sessionConfig"].options`
pub fn parse_session_config_options(session: &Value) -> DiscoveredConfig {
    let mut out = DiscoveredConfig {
        fetched_at: Some(chrono::Utc::now().to_rfc3339()),
        ..Default::default()
    };

    // 1) Standard configOptions
    if let Some(opts) = session
        .get("configOptions")
        .or_else(|| session.get("config_options"))
        .and_then(|v| v.as_array())
    {
        for opt in opts {
            let id = opt
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let category = opt
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let choices = parse_option_choices(opt);
            if category == "model" || id == "model" {
                out.model_config_id =
                    Some(if id.is_empty() { "model".into() } else { id.clone() });
                if !choices.is_empty() {
                    out.models = choices;
                }
            } else if category == "thought_level"
                || id == "thought_level"
                || id.contains("thought")
                || id.contains("reason")
            {
                out.thought_config_id = Some(if id.is_empty() {
                    "thought_level".into()
                } else {
                    id.clone()
                });
                if !choices.is_empty() {
                    out.thought_levels = choices;
                }
            } else if category == "mode" || id == "mode" {
                out.mode_config_id =
                    Some(if id.is_empty() { "mode".into() } else { id });
                if !choices.is_empty() {
                    out.modes = choices.clone();
                    // Grok-style "mode" is often effort/thinking, not coding mode.
                    if out.thought_levels.is_empty()
                        && choices.iter().any(|c| {
                            matches!(
                                c.value.as_str(),
                                "low" | "medium" | "high" | "minimal" | "default"
                            )
                        })
                    {
                        out.thought_levels = choices;
                        out.thought_config_id =
                            Some("mode".into()); // applied via session/set_mode
                    }
                }
            }
        }
    }

    // 2) Grok / Zed-style models object
    if let Some(models_obj) = session.get("models") {
        if let Some(available) = models_obj
            .get("availableModels")
            .or_else(|| models_obj.get("available_models"))
            .and_then(|v| v.as_array())
        {
            let mut models = Vec::new();
            let mut thought_from_meta = Vec::new();
            for m in available {
                let value = m
                    .get("modelId")
                    .or_else(|| m.get("model_id"))
                    .or_else(|| m.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if value.is_empty() {
                    continue;
                }
                let name = m
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(value.as_str())
                    .to_string();
                let description = m
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                models.push(ConfigChoice {
                    value,
                    name,
                    description,
                });

                if thought_from_meta.is_empty() {
                    if let Some(efforts) = m
                        .pointer("/_meta/reasoningEfforts")
                        .or_else(|| m.pointer("/_meta/reasoning_efforts"))
                        .and_then(|v| v.as_array())
                    {
                        for e in efforts {
                            let value = e
                                .get("value")
                                .or_else(|| e.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if value.is_empty() {
                                continue;
                            }
                            let name = e
                                .get("label")
                                .or_else(|| e.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or(value.as_str())
                                .to_string();
                            let description = e
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            thought_from_meta.push(ConfigChoice {
                                value,
                                name,
                                description,
                            });
                        }
                    }
                }
            }
            if !models.is_empty() {
                out.models = models;
                out.model_config_id = Some("model".into()); // use session/set_model
            }
            if out.thought_levels.is_empty() && !thought_from_meta.is_empty() {
                out.thought_levels = thought_from_meta;
                out.thought_config_id = Some("mode".into()); // session/set_mode
            }
        }
    }

    // 3) Grok _meta["x.ai/sessionConfig"].options
    if let Some(opts) = session
        .pointer("/_meta/x.ai/sessionConfig/options")
        .and_then(|v| v.as_array())
    {
        let mut models = Vec::new();
        let mut thoughts = Vec::new();
        for o in opts {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let category = o.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let label = o
                .get("label")
                .or_else(|| o.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or(id.as_str())
                .to_string();
            let description = o
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if id.is_empty() {
                continue;
            }
            let choice = ConfigChoice {
                value: id,
                name: label,
                description,
            };
            match category {
                "model" => models.push(choice),
                "mode" | "thought_level" => thoughts.push(choice),
                _ => {}
            }
        }
        if out.models.is_empty() && !models.is_empty() {
            out.models = models;
            out.model_config_id = Some("model".into());
        }
        if out.thought_levels.is_empty() && !thoughts.is_empty() {
            out.thought_levels = thoughts;
            out.thought_config_id = Some("mode".into());
        }
    }

    out
}

fn parse_option_choices(opt: &Value) -> Vec<ConfigChoice> {
    let Some(arr) = opt.get("options").and_then(|v| v.as_array()) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|c| {
            let value = c
                .get("value")
                .and_then(|v| v.as_str())
                .or_else(|| c.get("name").and_then(|v| v.as_str()))?
                .to_string();
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(value.as_str())
                .to_string();
            let description = c
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(ConfigChoice {
                value,
                name,
                description,
            })
        })
        .collect()
}

async fn apply_model(
    sid: &str,
    model: &str,
    discovered: &DiscoveredConfig,
    next_id: &AtomicU64,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    stdin: &Mutex<ChildStdin>,
    log: &LogFn,
) {
    // 1) Standard ACP
    let config_id = discovered
        .model_config_id
        .clone()
        .unwrap_or_else(|| "model".into());
    if rpc(
        "session/set_config_option",
        json!({ "sessionId": sid, "configId": config_id, "value": model }),
        next_id,
        pending,
        stdin,
        log,
    )
    .await
    .is_ok()
    {
        log(LogPiece::Line(format!(
            "✓ model set to {model} (session/set_config_option)"
        )));
        return;
    }
    // 2) Grok / Zed style
    match rpc(
        "session/set_model",
        json!({ "sessionId": sid, "modelId": model }),
        next_id,
        pending,
        stdin,
        log,
    )
    .await
    {
        Ok(_) => log(LogPiece::Line(format!(
            "✓ model set to {model} (session/set_model)"
        ))),
        Err(e) => log(LogPiece::Line(format!(
            "! set model failed: {e} (CLI -m may still apply)"
        ))),
    }
}

async fn apply_thought_level(
    sid: &str,
    level: &str,
    discovered: &DiscoveredConfig,
    next_id: &AtomicU64,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    stdin: &Mutex<ChildStdin>,
    log: &LogFn,
) {
    let config_id = discovered
        .thought_config_id
        .clone()
        .unwrap_or_else(|| "thought_level".into());
    if rpc(
        "session/set_config_option",
        json!({ "sessionId": sid, "configId": config_id, "value": level }),
        next_id,
        pending,
        stdin,
        log,
    )
    .await
    .is_ok()
    {
        log(LogPiece::Line(format!(
            "✓ thought_level set to {level} (session/set_config_option)"
        )));
        return;
    }
    // Grok: reasoning effort via session/set_mode { modeId }
    match rpc(
        "session/set_mode",
        json!({ "sessionId": sid, "modeId": level }),
        next_id,
        pending,
        stdin,
        log,
    )
    .await
    {
        Ok(_) => log(LogPiece::Line(format!(
            "✓ thought/effort set to {level} (session/set_mode)"
        ))),
        Err(e) => log(LogPiece::Line(format!(
            "! set thought_level failed: {e}"
        ))),
    }
}

/// Probe an ACP agent: initialize + session/new, return advertised config options.
/// Does not send a prompt. Used to populate model / thought_level pickers.
pub async fn discover_acp_config(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<DiscoveredConfig> {
    let log: LogFn = Arc::new(|_: LogPiece| {});
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn `{command}` for discovery failed"))?;
    let stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let _stderr = child.stderr.take();

    let stdin = Arc::new(Mutex::new(stdin));
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));

    let pending_r = pending.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(id) = msg.get("id").and_then(|i| {
                i.as_u64()
                    .or_else(|| i.as_i64().map(|x| x as u64))
                    .or_else(|| i.as_str().and_then(|s| s.parse().ok()))
            }) {
                if msg.get("result").is_some() || msg.get("error").is_some() {
                    let mut map = pending_r.lock().await;
                    if let Some(tx) = map.remove(&id) {
                        if let Some(err) = msg.get("error") {
                            let _ = tx.send(Err(anyhow!("rpc error: {err}")));
                        } else {
                            let _ =
                                tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                }
            }
            // Auto-approve permission requests during discovery if any
            if msg.get("method").and_then(|m| m.as_str()).is_some() && msg.get("id").is_some() {
                // ignore — discovery should not need tools
            }
        }
    });

    let _init = rpc(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true,
                "session": { "configOptions": { "boolean": {} } }
            },
            "clientInfo": {
                "name": "agent-fleet-discover",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await
    .context("initialize")?;

    if let Some(method_id) = pick_auth_method(&_init) {
        let _ = rpc(
            "authenticate",
            json!({ "methodId": method_id, "_meta": { "headless": true } }),
            &next_id,
            &pending,
            &stdin,
            &log,
        )
        .await;
    }

    let cwd_str = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .display()
        .to_string();
    let session = rpc(
        "session/new",
        json!({ "cwd": cwd_str, "mcpServers": [] }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await
    .context("session/new")?;

    let discovered = parse_session_config_options(&session);

    if let Some(sid) = session
        .get("sessionId")
        .or_else(|| session.get("session_id"))
        .and_then(|v| v.as_str())
    {
        let _ = rpc(
            "session/cancel",
            json!({ "sessionId": sid }),
            &next_id,
            &pending,
            &stdin,
            &log,
        )
        .await;
    }
    {
        let mut s = stdin.lock().await;
        let _ = s.shutdown().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(discovered)
}

pub async fn run_acp_task(req: AcpRunRequest) -> Result<AcpRunResult> {
    let log = req.log.clone();
    log(LogPiece::Line(format!(
        "▶ spawning `{} {}`",
        req.command,
        req.args.join(" ")
    )));
    log(LogPiece::Line(format!("  cwd: {}", req.cwd.display())));

    if !req.cwd.is_dir() {
        bail!("project path is not a directory: {}", req.cwd.display());
    }

    let mut cmd = Command::new(&req.command);
    cmd.args(&req.args)
        .current_dir(&req.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &req.env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn `{}` failed — is it installed?", req.command))?;

    let stdin = child.stdin.take().context("agent stdin")?;
    let stdout = child.stdout.take().context("agent stdout")?;
    let stderr = child.stderr.take().context("agent stderr")?;

    let log_err = log.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log_err(LogPiece::Line(format!("[stderr] {line}")));
        }
    });

    let stdin = Arc::new(Mutex::new(stdin));
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));
    let transcript = Arc::new(Mutex::new(TranscriptBuilder::default()));
    let auto_approve = req.auto_approve;

    let pending_r = pending.clone();
    let stdin_r = stdin.clone();
    let log_r = log.clone();
    let transcript_r = transcript.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    log_r(LogPiece::Line(format!("! bad json: {e} | {line}")));
                    continue;
                }
            };

            if let Some(id) = msg.get("id").and_then(|i| {
                i.as_u64()
                    .or_else(|| i.as_i64().map(|x| x as u64))
                    .or_else(|| i.as_str().and_then(|s| s.parse().ok()))
            }) {
                if msg.get("result").is_some() || msg.get("error").is_some() {
                    let mut map = pending_r.lock().await;
                    if let Some(tx) = map.remove(&id) {
                        if let Some(err) = msg.get("error") {
                            let _ = tx.send(Err(anyhow!("rpc error: {err}")));
                        } else {
                            let _ =
                                tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                    continue;
                }
            }

            if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                if msg.get("id").is_some() {
                    handle_agent_request(&msg, method, auto_approve, &stdin_r, &log_r).await;
                } else if method == "session/update" {
                    if let Some(update) = msg.pointer("/params/update") {
                        if let Some(piece) = format_update(update) {
                            {
                                let mut tr = transcript_r.lock().await;
                                match &piece {
                                    LogPiece::Stream { stream, text } => {
                                        tr.push_stream(*stream, text);
                                    }
                                    LogPiece::Line(line) => {
                                        tr.push_line(line);
                                    }
                                }
                            }
                            log_r(piece);
                        }
                    }
                } else if method.starts_with("_") {
                    // Vendor extensions (e.g. _x.ai/*) — skip noisy notify lines
                } else {
                    log_r(LogPiece::Line(format!("← notify {method}")));
                }
            }
        }
    });

    let init = rpc(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true,
                "session": { "configOptions": { "boolean": {} } }
            },
            "clientInfo": {
                "name": "agent-fleet",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await
    .context("initialize")?;
    log(LogPiece::Line("✓ initialized".into()));

    // Grok (and some other agents) require authenticate after initialize.
    // Docs: https://docs.x.ai/build/cli/headless-scripting
    if let Some(method_id) = pick_auth_method(&init) {
        match rpc(
            "authenticate",
            json!({
                "methodId": method_id,
                "_meta": { "headless": true }
            }),
            &next_id,
            &pending,
            &stdin,
            &log,
        )
        .await
        {
            Ok(_) => log(LogPiece::Line(format!("✓ authenticated via {method_id}"))),
            Err(e) => log(LogPiece::Line(format!("! authenticate ({method_id}) failed: {e}"))),
        }
    }

    let mcp = build_mcp_params(&req.mcp_servers);
    let cwd_str = req
        .cwd
        .canonicalize()
        .unwrap_or_else(|_| req.cwd.clone())
        .display()
        .to_string();

    let session = rpc(
        "session/new",
        json!({
            "cwd": cwd_str,
            "mcpServers": mcp,
        }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await
    .context("session/new")?;

    let session_id = session
        .get("sessionId")
        .or_else(|| session.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    log(LogPiece::Line(format!(
        "✓ session {}",
        session_id.as_deref().unwrap_or("(none)")
    )));

        let discovered = parse_session_config_options(&session);
    if !discovered.models.is_empty() {
        let names: Vec<_> = discovered.models.iter().map(|m| m.value.as_str()).collect();
        log(LogPiece::Line(format!(
            "• agent models: {}",
            names.join(", ")
        )));
    }
    if !discovered.thought_levels.is_empty() {
        let names: Vec<_> = discovered
            .thought_levels
            .iter()
            .map(|m| m.value.as_str())
            .collect();
        log(LogPiece::Line(format!(
            "• thought levels: {}",
            names.join(", ")
        )));
    }

    if let Some(sid) = &session_id {
        if let Some(model) = req.model.as_ref() {
            apply_model(
                sid,
                model,
                &discovered,
                &next_id,
                &pending,
                &stdin,
                &log,
            )
            .await;
        }
        if let Some(level) = req.thought_level.as_ref() {
            apply_thought_level(
                sid,
                level,
                &discovered,
                &next_id,
                &pending,
                &stdin,
                &log,
            )
            .await;
        }
    }

    let sid = session_id
        .clone()
        .ok_or_else(|| anyhow!("session/new returned no sessionId"))?;

    let full_prompt = compose_prompt(&req.system_prompt, &req.skills, &req.user_prompt, &req.cwd);
    // Always log outbound task text so UI/debug can verify the agent saw the full request.
    {
        let chars = full_prompt.chars().count();
        let preview: String = full_prompt.chars().take(500).collect();
        let suffix = if chars > 500 { "…" } else { "" };
        log(LogPiece::Line(format!(
            "→ session/prompt ({chars} chars):\n{preview}{suffix}"
        )));
    }

    let prompt_result = rpc(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [ { "type": "text", "text": full_prompt } ]
        }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await
    .context("session/prompt")?;

    let stop_reason = prompt_result
        .get("stopReason")
        .or_else(|| prompt_result.get("stop_reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    log(LogPiece::Line(format!(
        "✅ prompt complete ({})",
        stop_reason.as_deref().unwrap_or("ok")
    )));

    let _ = rpc(
        "session/cancel",
        json!({ "sessionId": sid }),
        &next_id,
        &pending,
        &stdin,
        &log,
    )
    .await;

    {
        let mut s = stdin.lock().await;
        let _ = s.shutdown().await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let _ = child.start_kill();
    let _ = child.wait().await;

    let transcript = transcript.lock().await.finish();
    Ok(AcpRunResult {
        session_id: Some(sid),
        stop_reason,
        transcript,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn build_mcp_stdio_and_http() {
        let servers = vec![
            McpServerConfig {
                name: "fs".into(),
                command: Some("/bin/mcp".into()),
                args: vec!["--stdio".into()],
                env: {
                    let mut m = HashMap::new();
                    m.insert("K".into(), "V".into());
                    m
                },
                r#type: None,
                url: None,
                headers: HashMap::new(),
            },
            McpServerConfig {
                name: "remote".into(),
                command: None,
                args: vec![],
                env: HashMap::new(),
                r#type: Some("http".into()),
                url: Some("https://example.com/mcp".into()),
                headers: {
                    let mut m = HashMap::new();
                    m.insert("Authorization".into(), "Bearer x".into());
                    m
                },
            },
        ];
        let v = build_mcp_params(&servers);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "fs");
        assert_eq!(arr[0]["command"], "/bin/mcp");
        assert_eq!(arr[0]["env"][0]["name"], "K");
        assert_eq!(arr[1]["type"], "http");
        assert_eq!(arr[1]["url"], "https://example.com/mcp");
    }

    #[test]
    fn compose_user_message_includes_title_and_prompt() {
        let msg = compose_user_message(
            "tell me a joke about trump and clawn",
            "you are professional comedian with 20 year experience in dark jokes",
        );
        assert!(msg.contains("tell me a joke about trump and clawn"));
        assert!(msg.contains("professional comedian"));
        // title first so the ask is obvious
        assert!(msg.starts_with("tell me a joke"));
    }

    #[test]
    fn compose_user_message_no_duplicate_title() {
        let msg = compose_user_message("hello", "hello world");
        assert_eq!(msg, "hello world");
    }

    #[test]
    fn compose_prompt_includes_system_skill_and_task() {
        let dir = tempfile_dir();
        let skill = dir.join("SKILL.md");
        std::fs::write(&skill, "do the thing carefully").unwrap();

        let text = compose_prompt(
            "system line",
            &[SkillRef {
                path: skill.display().to_string(),
                enabled: true,
            }, SkillRef {
                path: "disabled".into(),
                enabled: false,
            }],
            "user task",
            &dir,
        );
        assert!(text.contains("system line"));
        assert!(text.contains("do the thing carefully"));
        assert!(text.contains("## Task"));
        assert!(text.contains("user task"));
        assert!(!text.contains("### Skill `disabled`"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_prompt_skill_dir() {
        let dir = tempfile_dir();
        let skill_dir = dir.join("myskill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "from dir").unwrap();
        let text = compose_prompt(
            "",
            &[SkillRef {
                path: "myskill".into(),
                enabled: true,
            }],
            "go",
            &dir,
        );
        assert!(text.contains("from dir"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_update_message_and_tool() {
        let msg = format_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hi" }
        }));
        assert_eq!(
            msg,
            Some(LogPiece::Stream {
                stream: StreamKind::Agent,
                text: "hi".into()
            })
        );

        let thought = format_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "hmm" }
        }));
        match thought {
            Some(LogPiece::Stream {
                stream: StreamKind::Thought,
                text,
            }) => assert_eq!(text, "hmm"),
            other => panic!("unexpected {other:?}"),
        }

        let tool = format_update(&json!({
            "sessionUpdate": "tool_call",
            "title": "read_file"
        }));
        match tool {
            Some(LogPiece::Line(s)) => assert!(s.contains("read_file")),
            other => panic!("unexpected {other:?}"),
        }

        // User echo is skipped (we already log outbound prompt)
        assert!(format_update(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "echo" }
        }))
        .is_none());

        // High-frequency noise filtered
        assert!(format_update(&json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": []
        }))
        .is_none());
        assert!(format_update(&json!({
            "sessionUpdate": "usage_update"
        }))
        .is_none());
    }

    #[test]
    fn transcript_separates_streams() {
        let mut tr = TranscriptBuilder::default();
        tr.push_stream(StreamKind::Agent, "Hello");
        tr.push_stream(StreamKind::Agent, " world");
        tr.push_stream(StreamKind::Thought, "planning…");
        tr.push_line("🔧 tool_call: web_search");
        tr.push_stream(StreamKind::Agent, "Done.");
        let out = tr.finish();
        assert!(out.contains("── assistant ──"));
        assert!(out.contains("Hello world"));
        assert!(out.contains("── thought ──"));
        assert!(out.contains("planning…"));
        assert!(out.contains("🔧 tool_call: web_search"));
        assert!(out.matches("── assistant ──").count() >= 2);
        assert!(!out.contains("worldplanning"));
        assert!(!out.contains("planning…🔧"));
    }

    #[test]
    fn parse_config_options_models_and_thought() {
        let session = json!({
            "sessionId": "s1",
            "configOptions": [
                {
                    "id": "model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "a",
                    "options": [
                        {"value": "a", "name": "A"},
                        {"value": "b", "name": "B", "description": "big"}
                    ]
                },
                {
                    "id": "thought_level",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": "low",
                    "options": [
                        {"value": "low", "name": "Low"},
                        {"value": "high", "name": "High"}
                    ]
                }
            ]
        });
        let d = parse_session_config_options(&session);
        assert_eq!(d.models.len(), 2);
        assert_eq!(d.models[1].value, "b");
        assert_eq!(d.thought_levels.len(), 2);
        assert_eq!(d.model_config_id.as_deref(), Some("model"));
        assert_eq!(d.thought_config_id.as_deref(), Some("thought_level"));
    }

    #[test]
    fn parse_grok_models_object_and_reasoning_efforts() {
        let session = json!({
            "sessionId": "s-grok",
            "models": {
                "currentModelId": "grok-4.5",
                "availableModels": [
                    {
                        "modelId": "grok-4.5",
                        "name": "Grok 4.5",
                        "description": "frontier",
                        "_meta": {
                            "supportsReasoningEffort": true,
                            "reasoningEfforts": [
                                {"id": "high", "value": "high", "label": "High Effort"},
                                {"id": "medium", "value": "medium", "label": "Medium Effort"},
                                {"id": "low", "value": "low", "label": "Low Effort"}
                            ]
                        }
                    },
                    {
                        "modelId": "grok-build",
                        "name": "Grok Build"
                    }
                ]
            },
            "_meta": {
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                        {"id": "high", "category": "mode", "label": "High Effort", "selected": true}
                    ]
                }
            }
        });
        let d = parse_session_config_options(&session);
        assert_eq!(d.models.len(), 2);
        assert_eq!(d.models[0].value, "grok-4.5");
        assert_eq!(d.models[0].name, "Grok 4.5");
        assert_eq!(d.models[1].value, "grok-build");
        assert_eq!(d.thought_levels.len(), 3);
        assert!(d.thought_levels.iter().any(|t| t.value == "medium"));
    }

    #[test]
    fn format_update_empty_chunk_is_none() {
        assert!(format_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "" }
        }))
        .is_none());
    }


    #[test]
    fn pick_auth_method_prefers_api_key_when_env_set() {
        let init = json!({
            "authMethods": [
                {"id": "cached_token"},
                {"id": "xai.api_key"}
            ]
        });
        // Without env we still get something
        let picked = pick_auth_method(&init);
        assert!(picked.is_some());
        assert!(matches!(
            picked.as_deref(),
            Some("cached_token") | Some("xai.api_key")
        ));
        assert!(pick_auth_method(&json!({})).is_none());
    }

    #[tokio::test]
    async fn run_acp_against_mock_agent() {
        let mock = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mock_acp_agent.py");
        assert!(mock.is_file(), "missing mock agent at {}", mock.display());

        let cwd = tempfile_dir();
        let lines: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let lines_c = lines.clone();
        let log: LogFn = Arc::new(move |piece: LogPiece| {
            let s = match piece {
                LogPiece::Stream { text, .. } => text,
                LogPiece::Line(s) => s,
            };
            lines_c.lock().unwrap().push(s);
        });

        let result = run_acp_task(AcpRunRequest {
            command: "python3".into(),
            args: vec![mock.display().to_string()],
            env: HashMap::new(),
            cwd: cwd.clone(),
            model: Some("mock-strong".into()),
            thought_level: None,
            system_prompt: "sys".into(),
            user_prompt: "do a mock thing".into(),
            skills: vec![],
            mcp_servers: vec![],
            auto_approve: true,
            log,
        })
        .await
        .expect("mock ACP run should succeed");

        assert_eq!(result.session_id.as_deref(), Some("sess-mock-1"));
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert!(
            result.transcript.contains("hello from mock agent")
                || lines.lock().unwrap().iter().any(|l| l.contains("hello from mock")),
            "transcript/log missing agent text: {:?} / {:?}",
            result.transcript,
            lines.lock().unwrap()
        );

        let joined = lines.lock().unwrap().join("\n");
        assert!(joined.contains("initialize") || joined.contains("✓ initialized"));
        assert!(joined.contains("model set") || joined.contains("mock-strong") || joined.contains("set_config"));

        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    async fn run_acp_fails_on_missing_binary() {
        let cwd = tempfile_dir();
        let log: LogFn = Arc::new(|_: LogPiece| {});
        let err = run_acp_task(AcpRunRequest {
            command: "this-binary-definitely-does-not-exist-agent-fleet".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: cwd.clone(),
            model: None,
            thought_level: None,
            system_prompt: String::new(),
            user_prompt: "x".into(),
            skills: vec![],
            mcp_servers: vec![],
            auto_approve: true,
            log,
        })
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("spawn") || msg.contains("failed") || msg.contains("not found") || msg.contains("No such"),
            "unexpected error: {msg}"
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("af-acp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Live check against local `grok agent stdio` (no-op skip if grok missing).
    #[tokio::test]
    async fn live_discover_grok_models() {
        let grok_ok = std::process::Command::new("grok")
            .arg("--help")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !grok_ok {
            eprintln!("skip: grok not available");
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let cfg = discover_acp_config(
            "grok",
            &["agent".into(), "stdio".into()],
            &HashMap::new(),
            &cwd,
        )
        .await
        .expect("discover_acp_config against grok");
        eprintln!(
            "discovered models: {:?}",
            cfg.models.iter().map(|m| &m.value).collect::<Vec<_>>()
        );
        eprintln!(
            "thought levels: {:?}",
            cfg.thought_levels
                .iter()
                .map(|m| &m.value)
                .collect::<Vec<_>>()
        );
        assert!(
            !cfg.models.is_empty(),
            "expected grok to advertise models via models.availableModels, got empty"
        );
        assert!(
            cfg.models.iter().any(|m| m.value.contains("grok")),
            "expected a grok-* model id, got {:?}",
            cfg.models
        );
        assert!(
            !cfg.thought_levels.is_empty(),
            "expected reasoning efforts (high/medium/low)"
        );
    }
}
