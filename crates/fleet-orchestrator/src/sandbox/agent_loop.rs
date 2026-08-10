//! Control-plane coding-agent loop (true multi-agent).
//! LLM + file/bash tools scoped to the session workdir. API keys stay here.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::task::AbortHandle;

use super::SpawnRequest;

/// Resolved worker LLM after combining env (Grok only) + workspace settings (Ollama).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLlm {
    pub backend: Option<String>,
    pub model: Option<String>,
}

impl WorkerLlm {
    pub fn is_llm(&self) -> bool {
        self.backend.is_some()
    }
}

/// `FLEET_AGENT_BACKEND=grok` — host/env only, never a Settings value.
pub fn grok_env_enabled() -> bool {
    if cfg!(test) {
        return false;
    }
    matches!(
        std::env::var("FLEET_AGENT_BACKEND")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("grok" | "xai")
    )
}

fn grok_model() -> String {
    std::env::var("GROK_MODEL").unwrap_or_else(|_| "grok-4.5".into())
}

/// Normalize chat/plan provider ids to a worker backend.
pub fn normalize_provider(provider: &str) -> Option<String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "none" | "bash" => None,
        "grok" | "xai" => Some("grok".into()),
        "ollama" => Some("ollama".into()),
        "claude" | "anthropic" => Some("claude".into()),
        other => Some(other.to_string()),
    }
}

/// Workspace Settings / env only (no chat turn).
pub fn resolve_workspace_llm(fleet_llm: &str, fleet_llm_model: &str) -> WorkerLlm {
    resolve_worker_llm(None, None, fleet_llm, fleet_llm_model)
}

/// Plan/chat selection wins (same model as the composer). Then settings Ollama, then env Grok.
pub fn resolve_worker_llm(
    plan_provider: Option<&str>,
    plan_model: Option<&str>,
    fleet_llm: &str,
    fleet_llm_model: &str,
) -> WorkerLlm {
    if let Some(backend) = plan_provider.and_then(normalize_provider) {
        let model = plan_model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| match backend.as_str() {
                "grok" => Some(grok_model()),
                "ollama" => Some(
                    std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".into()),
                ),
                _ => None,
            });
        return WorkerLlm {
            backend: Some(backend),
            model,
        };
    }
    if grok_env_enabled() {
        return WorkerLlm {
            backend: Some("grok".into()),
            model: Some(grok_model()),
        };
    }
    let llm = fleet_llm.trim().to_ascii_lowercase();
    if llm == "ollama" {
        let model = fleet_llm_model.trim().to_string();
        let model = if model.is_empty() {
            std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".into())
        } else {
            model
        };
        return WorkerLlm {
            backend: Some("ollama".into()),
            model: Some(model),
        };
    }
    WorkerLlm {
        backend: None,
        model: None,
    }
}

/// @deprecated use [`resolve_worker_llm`]. Kept for callers that only check env Grok.
pub fn agent_backend() -> Option<String> {
    if grok_env_enabled() {
        Some("grok".into())
    } else {
        None
    }
}

pub struct AgentJob {
    abort: AbortHandle,
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl AgentJob {
    pub fn start(req: &SpawnRequest) -> Self {
        let work_dir = req.work_dir.clone();
        let timeout = req.timeout;
        let backend = req
            .agent_backend
            .clone()
            .unwrap_or_else(|| "grok".into());
        let model = req.agent_model.clone();
        let exit_code = Arc::new(Mutex::new(None));
        let exit2 = exit_code.clone();
        let handle = tokio::spawn(async move {
            let code = run_loop(&work_dir, timeout, &backend, model.as_deref())
                .await
                .unwrap_or(1);
            if let Ok(mut g) = exit2.lock() {
                *g = Some(code);
            }
        });
        Self {
            abort: handle.abort_handle(),
            exit_code,
        }
    }

    pub fn poll_exit(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|g| *g)
    }

    pub fn abort(&self) {
        self.abort.abort();
        if let Ok(mut g) = self.exit_code.lock()
            && g.is_none()
        {
            *g = Some(143);
        }
    }
}

async fn run_loop(
    work_dir: &Path,
    timeout: Duration,
    backend: &str,
    model: Option<&str>,
) -> Result<i32, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let persona = read_opt(work_dir.join("PERSONA.md")).await;
    let task = read_opt(work_dir.join("task.md")).await;
    let team = read_opt(work_dir.join("TEAM.md")).await;
    let system = format!(
        "{}\n\n# Fleet worker rules\n\
         You are ONE specialist on a multi-agent fleet. Other agents are working in parallel or waiting on you.\n\
         Read TEAM.md — it lists who is doing what. Stay in your lane.\n\
         Use handoff/ files from finished teammates. Do not redo their work.\n\
         Work only inside this workspace. Use tools to read/write files and run shell commands.\n\
         Do not request secrets. Do not leave the work directory.\n\
         When finished, write `result.md` summarizing what you did (the next agent will read it), then stop.\n\
         If you cannot complete the task, write result.md with blockers.\n",
        if persona.trim().is_empty() {
            "You are a competent software engineer.".into()
        } else {
            persona
        }
    );
    let mut user = if task.trim().is_empty() {
        "Inspect the workspace and complete the assigned work. Write result.md.".into()
    } else {
        format!("# Assigned task\n\n{task}")
    };
    if !team.trim().is_empty() {
        user.push_str("\n\n");
        user.push_str(&team);
    }

    let mut messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": user}),
    ];

    let max_rounds = std::env::var("FLEET_AGENT_MAX_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10usize)
        .clamp(1, 24);

    append_log(work_dir, "=== agent loop start ===\n").await;
    let mut last_text = String::new();
    let mut last_team = team;

    for round in 1..=max_rounds {
        if tokio::time::Instant::now() >= deadline {
            append_log(work_dir, "=== agent timeout ===\n").await;
            break;
        }
        let now_team = read_opt(work_dir.join("TEAM.md")).await;
        if round > 1 && now_team != last_team && !now_team.trim().is_empty() {
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "# Team update (live)\nTeammate statuses changed. Stay on your task.\n\n{now_team}"
                ),
            }));
            last_team = now_team;
        }
        let resp = match complete(&messages, backend, model).await {
            Ok(v) => v,
            Err(e) => {
                append_log(work_dir, &format!("=== llm error: {e} ===\n")).await;
                write_result(work_dir, &format!("Agent LLM error: {e}")).await;
                return Ok(1);
            }
        };
        let msg = resp
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or(json!({}));
        messages.push(msg.clone());

        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !content.trim().is_empty() {
            last_text = content.clone();
            append_log(work_dir, &format!("--- round {round} ---\n{content}\n")).await;
        }

        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if tool_calls.is_empty() {
            break;
        }
        for call in tool_calls {
            let id = call
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("call")
                .to_string();
            let name = call
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_raw = call
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let args: Value = serde_json::from_str(args_raw).unwrap_or(json!({}));
            let result = exec_tool(work_dir, &name, &args).await;
            append_log(
                work_dir,
                &format!("tool {name} -> {}\n", truncate(&result, 400)),
            )
            .await;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": result,
            }));
        }
    }

    if !work_dir.join("result.md").exists() && !last_text.trim().is_empty() {
        write_result(work_dir, &last_text).await;
    }
    append_log(work_dir, "=== agent loop done ===\n").await;
    Ok(0)
}

fn tools_schema() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List files in a relative directory (default .)",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 file relative to the workspace",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write a UTF-8 file relative to the workspace",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_bash",
                "description": "Run a bash command in the workspace (no secrets). Timeout 60s.",
                "parameters": {
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_team",
                "description": "Who else is on this fleet run: personas, tasks, status, and finished teammates' handoffs.",
                "parameters": { "type": "object", "properties": {} }
            }
        }
    ])
}

async fn exec_tool(root: &Path, name: &str, args: &Value) -> String {
    match name {
        "list_dir" => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let Ok(path) = safe_path(root, rel) else {
                return "error: path escapes workspace".into();
            };
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .flatten()
                        .map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            if e.path().is_dir() {
                                format!("{n}/")
                            } else {
                                n
                            }
                        })
                        .collect();
                    names.sort();
                    if names.is_empty() {
                        "(empty)".into()
                    } else {
                        names.join("\n")
                    }
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "read_file" => {
            let Some(rel) = args.get("path").and_then(|v| v.as_str()) else {
                return "error: path required".into();
            };
            let Ok(path) = safe_path(root, rel) else {
                return "error: path escapes workspace".into();
            };
            match std::fs::read_to_string(&path) {
                Ok(s) => truncate(&s, 24_000),
                Err(e) => format!("error: {e}"),
            }
        }
        "write_file" => {
            let Some(rel) = args.get("path").and_then(|v| v.as_str()) else {
                return "error: path required".into();
            };
            let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
                return "error: content required".into();
            };
            let Ok(path) = safe_path(root, rel) else {
                return "error: path escapes workspace".into();
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, content) {
                Ok(()) => format!("wrote {} bytes to {rel}", content.len()),
                Err(e) => format!("error: {e}"),
            }
        }
        "run_bash" => {
            let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
                return "error: command required".into();
            };
            match Command::new("bash")
                .arg("-lc")
                .arg(cmd)
                .current_dir(root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output()
                .await
            {
                Ok(out) => {
                    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
                    if !out.stderr.is_empty() {
                        s.push_str("\n[stderr]\n");
                        s.push_str(&String::from_utf8_lossy(&out.stderr));
                    }
                    if !out.status.success() {
                        s.push_str(&format!("\n[exit {}]", out.status));
                    }
                    truncate(&s, 16_000)
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "read_team" | "list_team" => {
            match std::fs::read_to_string(root.join("TEAM.md")) {
                Ok(s) if !s.trim().is_empty() => s,
                _ => "No TEAM.md yet. You are the only worker so far.".into(),
            }
        }
        other => format!("error: unknown tool {other}"),
    }
}

fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, ()> {
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    if rel.split('/').any(|p| p == "..") {
        return Err(());
    }
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let joined = root.join(rel);
    if joined.exists() {
        let canon = joined.canonicalize().map_err(|_| ())?;
        if canon.starts_with(&root) {
            Ok(canon)
        } else {
            Err(())
        }
    } else {
        Ok(joined)
    }
}

async fn complete(messages: &[Value], backend: &str, model: Option<&str>) -> Result<Value, String> {
    if matches!(backend, "claude" | "anthropic") {
        return Err(
            "Claude workers are not wired yet — pick Grok (or Ollama) in chat for now".into(),
        );
    }
    let (url, key, model) = match backend {
        "ollama" => {
            let base = std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".into())
                .trim_end_matches('/')
                .to_string();
            let model = model
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".into())
                });
            (format!("{base}/v1/chat/completions"), String::new(), model)
        }
        _ => {
            let key = grok_token()?;
            let model = model
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(grok_model);
            (
                "https://api.x.ai/v1/chat/completions".into(),
                key,
                model,
            )
        }
    };

    let mut req = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?
        .post(&url)
        .json(&json!({
            "model": model,
            "messages": messages,
            "tools": tools_schema(),
            "temperature": 0.2,
        }));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("llm {status}: {}", truncate(&body, 400)));
    }
    serde_json::from_str(&body).map_err(|e| format!("llm json: {e}"))
}

fn grok_token() -> Result<String, String> {
    if let Ok(k) = std::env::var("XAI_API_KEY").or_else(|_| std::env::var("GROK_API_KEY")) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Ok(k);
        }
    }
    let path = std::env::var("GROK_AUTH_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".grok/auth.json"))
                .unwrap_or_else(|_| PathBuf::from(".grok/auth.json"))
        });
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| format!("no XAI_API_KEY and missing {}", path.display()))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object() {
        for val in obj.values() {
            if let Some(k) = val.get("key").and_then(|x| x.as_str())
                && !k.trim().is_empty()
            {
                return Ok(k.to_string());
            }
        }
    }
    Err("no Grok token in auth.json — run grok login or set XAI_API_KEY".into())
}

async fn read_opt(p: PathBuf) -> String {
    tokio::fs::read_to_string(p).await.unwrap_or_default()
}

async fn append_log(work: &Path, s: &str) {
    let p = work.join("stdout.log");
    let prev = tokio::fs::read_to_string(&p).await.unwrap_or_default();
    let _ = tokio::fs::write(&p, format!("{prev}{s}")).await;
}

async fn write_result(work: &Path, text: &str) {
    let _ = tokio::fs::write(work.join("result.md"), text).await;
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}\n…(truncated)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_off_is_bash_only_in_tests() {
        let w = resolve_worker_llm(None, None, "off", "");
        assert!(w.backend.is_none());
    }

    #[test]
    fn settings_ollama_picks_model() {
        let w = resolve_worker_llm(None, None, "ollama", "qwen2.5-coder");
        assert_eq!(w.backend.as_deref(), Some("ollama"));
        assert_eq!(w.model.as_deref(), Some("qwen2.5-coder"));
    }

    #[test]
    fn chat_selection_wins_over_settings() {
        let w = resolve_worker_llm(
            Some("grok"),
            Some("grok-4.5-internal"),
            "ollama",
            "llama3.2",
        );
        assert_eq!(w.backend.as_deref(), Some("grok"));
        assert_eq!(w.model.as_deref(), Some("grok-4.5-internal"));
    }

    #[test]
    fn future_claude_provider_is_preserved() {
        let w = resolve_worker_llm(Some("claude"), Some("claude-opus-4"), "off", "");
        assert_eq!(w.backend.as_deref(), Some("claude"));
        assert_eq!(w.model.as_deref(), Some("claude-opus-4"));
    }
}
