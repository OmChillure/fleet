//! Control-plane coding-agent loop (true multi-agent).
//! LLM + file/bash tools scoped to the session workdir. API keys stay here.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::task::AbortHandle;

use super::SpawnRequest;
use super::team_chat::{
    self, DEFAULT_ASK_WAIT_SECS, MAX_ASK_WAIT_SECS, TeamTalk, copy_team_chat_into, format_inbox,
    load_team_talk, mark_seen_through, post_message, unread_for, wait_for_reply,
};

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

fn grok_model() -> Option<String> {
    std::env::var("GROK_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
                "grok" => grok_model(),
                "ollama" => std::env::var("OLLAMA_MODEL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
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
            model: grok_model(),
        };
    }
    let llm = fleet_llm.trim().to_ascii_lowercase();
    if llm == "ollama" {
        let model = fleet_llm_model.trim().to_string();
        let model = if model.is_empty() {
            std::env::var("OLLAMA_MODEL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            Some(model)
        };
        return WorkerLlm {
            backend: Some("ollama".into()),
            model,
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
    hb_abort: AbortHandle,
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl AgentJob {
    pub fn start(req: &SpawnRequest) -> Self {
        let work_dir = req.work_dir.clone();
        let timeout = req.timeout;
        let backend = req.agent_backend.clone().unwrap_or_else(|| "grok".into());
        let model = req.agent_model.clone();
        let talk = load_team_talk(&req.work_dir).or_else(|| infer_talk(req));
        let io = req.io.clone();
        let workspace_id = req.project_id;
        let exit_code = Arc::new(Mutex::new(None));
        let exit2 = exit_code.clone();
        let hb_dir = req.work_dir.clone();
        let hb = tokio::spawn(async move {
            loop {
                let _ = tokio::fs::write(hb_dir.join(".heartbeat"), Utc::now().to_rfc3339()).await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        let hb_abort = hb.abort_handle();
        let handle = tokio::spawn(async move {
            let code = run_loop(
                &work_dir,
                timeout,
                &backend,
                model.as_deref(),
                talk.as_ref(),
                io,
                workspace_id,
            )
            .await
            .unwrap_or(1);
            hb.abort();
            if let Ok(mut g) = exit2.lock() {
                *g = Some(code);
            }
        });
        Self {
            abort: handle.abort_handle(),
            hb_abort,
            exit_code,
        }
    }

    pub fn poll_exit(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|g| *g)
    }

    pub fn abort(&self) {
        self.hb_abort.abort();
        self.abort.abort();
        if let Ok(mut g) = self.exit_code.lock()
            && g.is_none()
        {
            *g = Some(143);
        }
    }
}

fn infer_talk(req: &SpawnRequest) -> Option<TeamTalk> {
    if req.plan_task_id.trim().is_empty() {
        return None;
    }
    let cur = req
        .work_dir
        .canonicalize()
        .unwrap_or_else(|_| req.work_dir.clone());
    // …/projects/{pid}/sessions/{sid}
    cur.file_name()?;
    let sessions = cur.parent()?;
    if sessions.file_name()?.to_str()? != "sessions" {
        return None;
    }
    let proj = sessions.parent()?;
    if proj.file_name()?.to_str()? != req.project_id.to_string() {
        return None;
    }
    let projects = proj.parent()?;
    if projects.file_name()?.to_str()? != "projects" {
        return None;
    }
    let data_dir = projects.parent()?;
    Some(TeamTalk::from_spawn(
        data_dir,
        req.project_id,
        req.run_id,
        req.plan_task_id.clone(),
        req.worker_profile.clone(),
    ))
}

async fn run_loop(
    work_dir: &Path,
    timeout: Duration,
    backend: &str,
    model: Option<&str>,
    talk: Option<&TeamTalk>,
    io: super::io::IntegrationHandle,
    workspace_id: uuid::Uuid,
) -> Result<i32, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let persona = read_opt(work_dir.join("PERSONA.md")).await;
    let task = read_opt(work_dir.join("task.md")).await;
    let run_ctx = read_opt(work_dir.join("RUN.md")).await;
    let team = read_opt(work_dir.join("TEAM.md")).await;
    let me = talk.map(|t| t.me.as_str()).unwrap_or("this-task");
    let system = format!(
        "{}\n\n# Fleet worker rules\n\
         You are ONE specialist on a multi-agent fleet. Your id is `{me}`.\n\
         Other agents are working in parallel or waiting on you. You can TALK to them.\n\
         Read TEAM.md — who is doing what. Read TEAM_CHAT.md — what they already said.\n\
         When you need a contract, path, port, or decision from a live teammate, call `ask_teammate` (they will see it mid-run and can reply). Use `send_message` for FYIs (`to` = their id or `*`).\n\
         If a teammate asks you something, answer before you continue your own task.\n\
         Shared artifacts are in `work/`. Read that first. Put anything the next agent needs there.\n\
         If you are reviewing and the work is not acceptable, call `request_changes` with a concrete reason (they will be sent back to fix it).\n\
         Use handoff/ files from finished teammates. Do not redo their work.\n\
         Work only inside this workspace. Use tools to read/write files and run shell commands.\n\
         To talk to connected apps call `use_integration` (provider, operation, args). The control plane decrypts; you never see tokens. If an op is unknown the error lists valid ops.\n\
         Do only what the spec asks. Do not assume you must write code or open a PR.\n\
         Do not request secrets. Do not leave the work directory.\n\
         Read RUN.md — the whole board, your acceptance, and shared files.\n\
         When finished, write `result.md` summarizing what you did (the next agent will read it), then stop.\n\
         If you are reviewing and the work is not acceptable, call `request_changes`.\n\
         The control plane validates result.md and any acceptance lines on the task — nothing else.\n\
         If you cannot complete the task, write result.md with blockers.\n",
        if persona.trim().is_empty() {
            "You are a competent specialist on a multi-agent team.".into()
        } else {
            persona
        }
    );
    let mut user = if task.trim().is_empty() {
        "Inspect the workspace and complete the assigned work. Write result.md.".into()
    } else {
        format!("# Assigned task\n\n{task}")
    };
    if !run_ctx.trim().is_empty() {
        user.push_str("\n\n");
        user.push_str(&run_ctx);
    }
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
    emit_live(work_dir, "start", "Agent started thinking.").await;
    let mut last_text = String::new();
    let mut last_team = team;

    for round in 1..=max_rounds {
        if tokio::time::Instant::now() >= deadline {
            append_log(work_dir, "=== agent timeout ===\n").await;
            emit_live(work_dir, "timeout", "Timed out.").await;
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
        if let Some(talk) = talk {
            copy_team_chat_into(work_dir, talk).await;
            let inbox = unread_for(talk).await;
            if !inbox.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": format_inbox(&inbox),
                }));
                mark_seen_through(talk, &inbox).await;
                append_log(
                    work_dir,
                    &format!("=== team inbox: {} message(s) ===\n", inbox.len()),
                )
                .await;
                emit_live(
                    work_dir,
                    "inbox",
                    &format!("Got {} message(s) from teammates.", inbox.len()),
                )
                .await;
            }
        }
        let resp = match complete(&messages, backend, model).await {
            Ok(v) => v,
            Err(e) => {
                append_log(work_dir, &format!("=== llm error: {e} ===\n")).await;
                emit_live(work_dir, "error", &format!("LLM error: {e}")).await;
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
            emit_live(work_dir, "think", &truncate(&content, 800)).await;
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
            emit_live(work_dir, "tool", &name).await;
            let result = exec_tool(work_dir, talk, &name, &args, io.get(), workspace_id).await;
            append_log(
                work_dir,
                &format!("tool {name} -> {}\n", truncate(&result, 400)),
            )
            .await;
            emit_live(
                work_dir,
                "tool_result",
                &format!("{name}: {}", truncate(&result, 240)),
            )
            .await;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": result,
            }));
            if name == "use_integration"
                && args.get("operation").and_then(|v| v.as_str()) == Some("create_pr")
                && shipped_in_tool_result(&result)
            {
                write_result(work_dir, &format!("Shipped.\n\n{result}\n")).await;
                append_log(work_dir, "=== ship detected, exiting ===\n").await;
                emit_live(work_dir, "done", "PR opened — stopping.").await;
                return Ok(0);
            }
        }
    }

    if !work_dir.join("result.md").exists() && !last_text.trim().is_empty() {
        write_result(work_dir, &last_text).await;
    }
    append_log(work_dir, "=== agent loop done ===\n").await;
    emit_live(work_dir, "done", "Finished this turn.").await;
    Ok(0)
}

#[allow(dead_code)]
fn is_ship_role(plan_task_id: &str, worker_profile: &str) -> bool {
    let id = plan_task_id.to_ascii_lowercase();
    let p = worker_profile.to_ascii_lowercase();
    id == "ship" || p.contains("ship") || p.contains("devops")
}

fn shipped_in_tool_result(result: &str) -> bool {
    let t = result.to_ascii_lowercase();
    (t.contains("\"ok\":true") || t.contains("\"ok\": true"))
        && (t.contains("/pull/")
            || t.contains("html_url")
            || t.contains("create_pr")
            || t.contains("github.com"))
        && !t.contains("listed 0 prs")
}

fn extract_github_pr_url(text: &str) -> Option<String> {
    let marker = "https://github.com/";
    let mut rest = text;
    while let Some(i) = rest.find(marker) {
        let s = &rest[i..];
        let end = s
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '<' | '>'))
            .unwrap_or(s.len());
        let url = s[..end].trim_end_matches(['.', ',', ';', ':', '/']);
        if url.contains("/pull/") {
            return Some(url.to_string());
        }
        rest = &s[marker.len()..];
    }
    None
}

fn find_existing_pr_url(root: &Path) -> Option<String> {
    let mut paths = vec![
        root.join("result.md"),
        root.join("SHIP.md"),
        root.join("work").join("SHIP.md"),
        root.join("work").join("review_notes.md"),
        root.join("handoff").join("result.md"),
    ];
    for dir in [root.to_path_buf(), root.join("work"), root.join("handoff")] {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                paths.push(p);
            }
        }
    }
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if let Some(url) = extract_github_pr_url(&text) {
            return Some(url);
        }
    }
    None
}

fn find_owner_repo(text: &str) -> Option<(String, String)> {
    if let Some(url) = extract_github_pr_url(text) {
        let rest = url.trim_start_matches("https://github.com/");
        let mut it = rest.split('/');
        if let (Some(owner), Some(repo)) = (it.next(), it.next()) {
            if !owner.is_empty() && !repo.is_empty() {
                return Some((owner.to_string(), repo.to_string()));
            }
        }
    }
    for raw in text.split_whitespace() {
        let t = raw
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '-' && c != '_');
        if t.starts_with("feature/") || t.starts_with("fleet/") || t.starts_with("agent-fleet/") {
            continue;
        }
        let mut it = t.split('/');
        let owner = it.next().unwrap_or("");
        let repo = it.next().unwrap_or("");
        if it.next().is_none()
            && owner.len() >= 2
            && repo.len() >= 2
            && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    None
}

fn find_branch(text: &str) -> String {
    for raw in text.split_whitespace() {
        let t = raw.trim_matches(|c: char| c == '`' || c == '"' || c == '\'' || c == ',');
        if t.starts_with("feature/") || t.starts_with("fleet/") || t.starts_with("agent-fleet/") {
            return t.to_string();
        }
    }
    "agent-fleet/changes".into()
}

fn skip_ship_name(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if matches!(
        name,
        "node_modules"
            | "target"
            | "PERSONA.md"
            | "TEAM.md"
            | "TEAM_CHAT.md"
            | "task.md"
            | "result.md"
            | "live.jsonl"
            | "RUN.md"
    ) {
        return true;
    }
    let n = name.to_ascii_lowercase();
    n.ends_with(".json")
        && (n.contains("commit")
            || n.contains("ship")
            || n.contains("payload")
            || n.contains("_args"))
}

fn collect_ship_files(root: &Path) -> Vec<(String, String)> {
    let work = root.join("work");
    let base = if work.is_dir() {
        work
    } else {
        root.to_path_buf()
    };
    let mut out = Vec::new();
    fn walk(dir: &Path, rel: &Path, out: &mut Vec<(String, String)>) {
        if out.len() >= 40 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            if out.len() >= 40 {
                return;
            }
            let name = e.file_name();
            let name_s = name.to_string_lossy();
            if skip_ship_name(&name_s) {
                continue;
            }
            let path = e.path();
            let child_rel = rel.join(&name);
            if path.is_dir() {
                walk(&path, &child_rel, out);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes.len() > 200_000 || bytes.contains(&0) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            out.push((child_rel.to_string_lossy().replace('\\', "/"), text));
        }
    }
    // Source trees first so the 40-file cap is not eaten by helper junk.
    for prefer in ["app", "src", "lib"] {
        let d = base.join(prefer);
        if d.is_dir() {
            walk(&d, Path::new(prefer), &mut out);
        }
    }
    walk(&base, Path::new(""), &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn pr_url_from_data(data: &Value) -> Option<String> {
    if let Some(u) = data
        .get("html_url")
        .or_else(|| data.get("url"))
        .and_then(|v| v.as_str())
        .filter(|s| s.contains("github.com") || s.contains("/pull/"))
    {
        return Some(u.to_string());
    }
    if let Some(items) = data
        .get("items")
        .or_else(|| data.get("prs"))
        .and_then(|v| v.as_array())
    {
        for item in items {
            if let Some(u) = item
                .get("html_url")
                .or_else(|| item.get("url"))
                .and_then(|v| v.as_str())
            {
                if u.contains("/pull/") {
                    return Some(u.to_string());
                }
            }
        }
    }
    data.get("items")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|item| {
            let n = item.get("number").and_then(|v| v.as_i64())?;
            let owner = item
                .pointer("/head/repo/owner/login")
                .or_else(|| item.pointer("/base/repo/owner/login"))
                .and_then(|v| v.as_str())?;
            let repo = item
                .pointer("/head/repo/name")
                .or_else(|| item.pointer("/base/repo/name"))
                .and_then(|v| v.as_str())?;
            Some(format!("https://github.com/{owner}/{repo}/pull/{n}"))
        })
}

async fn run_ship(
    work_dir: &Path,
    io: super::io::IntegrationHandle,
    workspace_id: uuid::Uuid,
    spec: &str,
) -> Result<i32, String> {
    let task = read_opt(work_dir.join("task.md")).await;
    let run_ctx = read_opt(work_dir.join("RUN.md")).await;
    let team = read_opt(work_dir.join("TEAM.md")).await;
    let blob = format!("{spec}\n{task}\n{run_ctx}\n{team}");
    emit_live(
        work_dir,
        "start",
        "Shipping work/ to GitHub (no extra LLM loop).",
    )
    .await;

    if let Some(url) = find_existing_pr_url(work_dir) {
        write_result(
            work_dir,
            &format!(
                "Shipped.\n\nPR: {url}\n\nReused an existing pull request from the workspace.\n"
            ),
        )
        .await;
        emit_live(work_dir, "done", &format!("Opened {url}")).await;
        return Ok(0);
    }

    let Some(inv) = io.get() else {
        write_result(
            work_dir,
            "Ship failed: no control-plane GitHub proxy on this worker.\n",
        )
        .await;
        return Ok(1);
    };
    let Some((owner, repo)) = find_owner_repo(&blob) else {
        write_result(
            work_dir,
            "Ship failed: no owner/repo in the task spec (expected e.g. OmChillure/test).\n",
        )
        .await;
        return Ok(1);
    };

    emit_live(work_dir, "tool", "list_prs").await;
    let listed = inv
        .invoke(
            workspace_id,
            "github".into(),
            "list_prs".into(),
            json!({ "owner": owner, "repo": repo, "state": "open" }),
        )
        .await;
    if listed.ok {
        if let Some(url) =
            pr_url_from_data(&listed.data).or_else(|| extract_github_pr_url(&listed.summary))
        {
            write_result(
                work_dir,
                &format!("Shipped {owner}/{repo}.\n\nPR: {url}\n\nAlready open on GitHub.\n"),
            )
            .await;
            emit_live(work_dir, "done", &format!("Opened {url}")).await;
            return Ok(0);
        }
    }

    let files = collect_ship_files(work_dir);
    if files.is_empty() {
        write_result(work_dir, "Ship failed: work/ has no files to commit.\n").await;
        return Ok(1);
    }
    let branch = find_branch(&blob);
    emit_live(
        work_dir,
        "think",
        &format!(
            "Committing {} files to {owner}/{repo}@{branch}",
            files.len()
        ),
    )
    .await;
    let file_json: Vec<Value> = files
        .iter()
        .map(|(p, c)| json!({ "path": p, "content": c }))
        .collect();
    let commit = inv
        .invoke(
            workspace_id,
            "github".into(),
            "commit_files".into(),
            json!({
                "owner": owner,
                "repo": repo,
                "branch": branch,
                "message": "feat: apply fleet implementer work",
                "files": file_json,
            }),
        )
        .await;
    emit_live(work_dir, "tool", "commit_files").await;
    if !commit.ok {
        write_result(
            work_dir,
            &format!("Ship commit failed: {}\n", commit.summary),
        )
        .await;
        emit_live(work_dir, "error", &commit.summary).await;
        return Ok(1);
    }
    emit_live(work_dir, "think", "Opening pull request.").await;
    let pr = inv
        .invoke(
            workspace_id,
            "github".into(),
            "create_pr".into(),
            json!({
                "owner": owner,
                "repo": repo,
                "head": branch,
                "base": "main",
                "title": format!("Fleet: {branch}"),
                "body": "Opened by Agent Fleet ship step from work/.",
            }),
        )
        .await;
    emit_live(work_dir, "tool", "create_pr").await;
    let mut url = pr_url_from_data(&pr.data).unwrap_or_default();
    if url.is_empty() {
        url = extract_github_pr_url(&pr.summary).unwrap_or_default();
    }
    if !pr.ok && url.is_empty() {
        let again = inv
            .invoke(
                workspace_id,
                "github".into(),
                "list_prs".into(),
                json!({ "owner": owner, "repo": repo, "state": "open" }),
            )
            .await;
        url = pr_url_from_data(&again.data)
            .or_else(|| extract_github_pr_url(&again.summary))
            .unwrap_or_default();
    }
    if url.is_empty() && !pr.ok {
        write_result(work_dir, &format!("Ship PR failed: {}\n", pr.summary)).await;
        emit_live(work_dir, "error", &pr.summary).await;
        return Ok(1);
    }
    let link = if url.is_empty() {
        format!("https://github.com/{owner}/{repo}/pulls")
    } else {
        url
    };
    write_result(
        work_dir,
        &format!(
            "Shipped {owner}/{repo} on `{branch}`.\n\nPR: {link}\n\n{}\n",
            pr.summary
        ),
    )
    .await;
    emit_live(work_dir, "done", &format!("Opened {link}")).await;
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
        },
        {
            "type": "function",
            "function": {
                "name": "send_message",
                "description": "Talk to a live teammate (or the whole team). They see it on their next thinking round. Use their plan task id from TEAM.md (e.g. t1) or to='*' for everyone. If you are answering an ask, pass reply_to = their message id.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {
                            "type": "string",
                            "description": "Teammate plan task id, or * / team / all"
                        },
                        "body": { "type": "string", "description": "Message text" },
                        "reply_to": {
                            "type": "string",
                            "description": "Optional message id you are answering"
                        }
                    },
                    "required": ["to", "body"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_messages",
                "description": "Pull unread (or all) team chat messages. New messages are also injected automatically each round.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "all": {
                            "type": "boolean",
                            "description": "If true, return the full run transcript, not just unread"
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "request_changes",
                "description": "Reviewer tool: send one or more implementers back to rework. They become ready again with your notes. Use when the work fails review. Then write result.md and stop.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {
                            "type": "string",
                            "description": "Plan task id to bounce (e.g. t1). Omit to bounce everyone you depend on."
                        },
                        "targets": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of plan task ids"
                        },
                        "reason": {
                            "type": "string",
                            "description": "What is wrong and what to change"
                        }
                    },
                    "required": ["reason"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask_teammate",
                "description": "Ask a specific live teammate a question and wait for their reply. Use when you are blocked on a contract, path, or decision they own. They will see the ask mid-run.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {
                            "type": "string",
                            "description": "Their plan task id from TEAM.md (not *)"
                        },
                        "body": { "type": "string", "description": "The question" },
                        "wait_seconds": {
                            "type": "integer",
                            "description": "How long to wait (default 45, max 90)"
                        }
                    },
                    "required": ["to", "body"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "use_integration",
                "description": "Call a connected app on the CONTROL PLANE (Slack, Notion, GitHub, GitLab, Jira, Linear, Teams, Trello, Asana, Drive, Figma, Discord, …). Tokens never enter this workspace. Pass provider + allowlisted operation + args.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string" },
                        "operation": { "type": "string" },
                        "args": { "type": "object" }
                    },
                    "required": ["provider", "operation"]
                }
            }
        }
    ])
}

async fn exec_tool(
    root: &Path,
    talk: Option<&TeamTalk>,
    name: &str,
    args: &Value,
    io: Option<std::sync::Arc<dyn super::io::IntegrationInvoker>>,
    workspace_id: uuid::Uuid,
) -> String {
    match name {
        "use_integration" | "integration_query" => {
            let Some(inv) = io else {
                return "error: no control-plane integration proxy on this worker".into();
            };
            let Some(provider) = args.get("provider").and_then(|v| v.as_str()) else {
                return "error: provider required".into();
            };
            let Some(operation) = args.get("operation").and_then(|v| v.as_str()) else {
                return "error: operation required".into();
            };
            let tool_args = args.get("args").cloned().unwrap_or(json!({}));
            let res = inv
                .invoke(
                    workspace_id,
                    provider.to_string(),
                    operation.to_string(),
                    tool_args,
                )
                .await;
            let payload = json!({
                "ok": res.ok,
                "summary": res.summary,
                "data": res.data,
            });
            serde_json::to_string(&payload).unwrap_or_else(|_| res.summary)
        }
        "list_dir" => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let Ok(path) = safe_path(root, rel) else {
                return "error: path escapes workspace".into();
            };
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .flatten()
                        .take(400)
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
            run_bash_limited(root, cmd, Duration::from_secs(60)).await
        }
        "read_team" | "list_team" => match std::fs::read_to_string(root.join("TEAM.md")) {
            Ok(s) if !s.trim().is_empty() => s,
            _ => "No TEAM.md yet. You are the only worker so far.".into(),
        },
        "send_message" | "message_teammate" => {
            let Some(talk) = talk else {
                return "error: no team chat on this run".into();
            };
            let Some(to) = args.get("to").and_then(|v| v.as_str()) else {
                return "error: to required".into();
            };
            let Some(body) = args.get("body").and_then(|v| v.as_str()) else {
                return "error: body required".into();
            };
            let reply_to = args
                .get("reply_to")
                .or_else(|| args.get("in_reply_to"))
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok());
            let kind = if reply_to.is_some() { "reply" } else { "say" };
            match post_message(talk, to, kind, body, reply_to).await {
                Ok(m) => {
                    copy_team_chat_into(root, talk).await;
                    format!(
                        "sent {} to `{}` (id `{}`, seq {})",
                        m.kind, m.to, m.id, m.seq
                    )
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "read_messages" | "read_team_chat" => {
            let Some(talk) = talk else {
                return "error: no team chat on this run".into();
            };
            let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
            let msgs = if all {
                team_chat::list_messages(talk).await
            } else {
                unread_for(talk).await
            };
            if !all {
                mark_seen_through(talk, &msgs).await;
            }
            copy_team_chat_into(root, talk).await;
            format_inbox(&msgs)
        }
        "request_changes" => {
            let Some(reason) = args.get("reason").and_then(|v| v.as_str()) else {
                return "error: reason required".into();
            };
            let mut targets: Vec<String> = args
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(to) = args.get("to").and_then(|v| v.as_str()) {
                let t = crate::sandbox::team_chat::normalize_to(to);
                if t != "*" && !targets.iter().any(|x| x == &t) {
                    targets.push(t);
                }
            }
            let req = crate::sandbox::share::ChangesRequest {
                targets: targets.clone(),
                reason: reason.to_string(),
            };
            if let Err(e) = crate::sandbox::share::write_changes_request(root, &req) {
                return format!("error: {e}");
            }
            if let Some(talk) = talk {
                let to = targets.first().map(|s| s.as_str()).unwrap_or("*");
                let _ = post_message(
                    talk,
                    to,
                    "ask",
                    &format!("Changes requested: {reason}"),
                    None,
                )
                .await;
                copy_team_chat_into(root, talk).await;
            }
            format!(
                "recorded changes request for [{}]. Write result.md and stop — they will be requeued.",
                if targets.is_empty() {
                    "your dependencies".into()
                } else {
                    targets.join(", ")
                }
            )
        }
        "ask_teammate" => {
            let Some(talk) = talk else {
                return "error: no team chat on this run".into();
            };
            let Some(to) = args.get("to").and_then(|v| v.as_str()) else {
                return "error: to required".into();
            };
            let Some(body) = args.get("body").and_then(|v| v.as_str()) else {
                return "error: body required".into();
            };
            let wait_s = args
                .get("wait_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_ASK_WAIT_SECS)
                .clamp(3, MAX_ASK_WAIT_SECS);
            let to_n = team_chat::normalize_to(to);
            if to_n == "*" || to_n.eq_ignore_ascii_case(&talk.me) {
                return "error: ask a specific other teammate id from TEAM.md".into();
            }
            let ask = match post_message(talk, &to_n, "ask", body, None).await {
                Ok(m) => m,
                Err(e) => return format!("error: {e}"),
            };
            copy_team_chat_into(root, talk).await;
            match wait_for_reply(talk, ask.id, &to_n, Duration::from_secs(wait_s)).await {
                Some(reply) => {
                    copy_team_chat_into(root, talk).await;
                    format!(
                        "reply from `{}` (id `{}`):\n{}",
                        reply.from, reply.id, reply.body
                    )
                }
                None => format!(
                    "no reply from `{to_n}` after {wait_s}s (ask id `{}`). They may still be in an LLM round. Continue with a reasonable default and note it in result.md, or ask again later.",
                    ask.id
                ),
            }
        }
        other => format!("error: unknown tool {other}"),
    }
}

fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, ()> {
    let rel = rel.trim().trim_start_matches(['/', '\\']);
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    if rel.split(['/', '\\']).any(|p| p == "..") {
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
        let mut ancestor = joined.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or(())?;
        }
        let ancestor = ancestor.canonicalize().map_err(|_| ())?;
        if ancestor.starts_with(&root) {
            Ok(joined)
        } else {
            Err(())
        }
    }
}

async fn run_bash_limited(root: &Path, cmd: &str, timeout: Duration) -> String {
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(cmd)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("LANG", "C.UTF-8");
    let child = command.spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => return format!("error: {e}"),
    };
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
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
        Ok(Err(e)) => format!("error: {e}"),
        Err(_) => format!("error: timed out after {}s", timeout.as_secs()),
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
                .or_else(|| {
                    std::env::var("OLLAMA_MODEL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
                .ok_or_else(|| "no Ollama model: set OLLAMA_MODEL or pick one".to_string())?;
            (format!("{base}/v1/chat/completions"), String::new(), model)
        }
        _ => {
            let key = grok_token()?;
            let model = model
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(grok_model)
                .ok_or_else(|| "no Grok model: set GROK_MODEL or pick one".to_string())?;
            ("https://api.x.ai/v1/chat/completions".into(), key, model)
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

const LOG_CAP_BYTES: usize = 1_500_000;

async fn append_capped(path: &Path, chunk: &[u8], cap: usize) {
    use tokio::io::AsyncWriteExt;
    if chunk.is_empty() {
        return;
    }
    let len = tokio::fs::metadata(path)
        .await
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    if len.saturating_add(chunk.len()) > cap {
        let prev = tokio::fs::read(path).await.unwrap_or_default();
        let keep = cap / 2;
        let start = prev.len().saturating_sub(keep);
        let start = prev[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| start + i + 1)
            .unwrap_or(start);
        let mut out = b"...(log truncated)\n".to_vec();
        if start < prev.len() {
            out.extend_from_slice(&prev[start..]);
        }
        out.extend_from_slice(chunk);
        let _ = tokio::fs::write(path, out).await;
        return;
    }
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(mut f) => {
            let _ = f.write_all(chunk).await;
        }
        Err(_) => {
            let _ = tokio::fs::write(path, chunk).await;
        }
    }
}

async fn append_log(work: &Path, s: &str) {
    append_capped(&work.join("stdout.log"), s.as_bytes(), LOG_CAP_BYTES).await;
}

async fn emit_live(work: &Path, kind: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "kind": kind,
        "text": text,
    });
    let chunk = format!("{line}\n");
    append_capped(&work.join("live.jsonl"), chunk.as_bytes(), LOG_CAP_BYTES).await;
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

    #[test]
    fn plan_grok_wins_over_workspace_ollama_and_off() {
        let w = resolve_worker_llm(Some("grok"), Some("grok-4.5"), "ollama", "llama3.2");
        assert_eq!(
            w.backend.as_deref(),
            Some("grok"),
            "plan LLM is the product source of truth, not Settings / API env"
        );
        assert_eq!(w.model.as_deref(), Some("grok-4.5"));
        let off = resolve_worker_llm(Some("grok"), Some("grok-4.5"), "off", "");
        assert_eq!(off.backend.as_deref(), Some("grok"));
    }

    #[test]
    fn normalize_provider_maps_aliases_and_off() {
        assert_eq!(normalize_provider(""), None);
        assert_eq!(normalize_provider("off"), None);
        assert_eq!(normalize_provider("none"), None);
        assert_eq!(normalize_provider("bash"), None);
        assert_eq!(normalize_provider("xai").as_deref(), Some("grok"));
        assert_eq!(normalize_provider("GROK").as_deref(), Some("grok"));
        assert_eq!(normalize_provider("ollama").as_deref(), Some("ollama"));
    }

    #[test]
    fn grok_env_is_forced_off_in_unit_tests() {
        assert!(!grok_env_enabled());
        assert!(agent_backend().is_none());
        let w = resolve_workspace_llm("off", "ignored");
        assert!(!w.is_llm());
    }

    #[test]
    fn safe_path_rejects_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(safe_path(dir.path(), "../etc/passwd").is_err());
        assert!(safe_path(dir.path(), "ok/file.txt").is_ok());
        assert!(
            safe_path(dir.path(), "")
                .unwrap()
                .ends_with(dir.path().file_name().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn safe_path_rejects_symlink_dir_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = dir.path().join("out");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(safe_path(dir.path(), "out/secret").is_err());
    }

    #[tokio::test]
    async fn run_bash_times_out_and_kills() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_bash_limited(dir.path(), "sleep 8", Duration::from_millis(200)).await;
        assert!(out.contains("timed out"), "{out}");
    }

    #[tokio::test]
    async fn append_capped_does_not_reread_on_small_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        append_capped(&p, b"hello\n", 100).await;
        append_capped(&p, b"world\n", 100).await;
        let body = tokio::fs::read_to_string(&p).await.unwrap();
        assert_eq!(body, "hello\nworld\n");
    }

    #[test]
    fn truncate_appends_marker() {
        let s = truncate("abcdef", 3);
        assert!(s.starts_with("abc"));
        assert!(s.contains("truncated"));
        assert_eq!(truncate("hi", 10), "hi");
    }

    async fn tool(
        root: &std::path::Path,
        talk: Option<&TeamTalk>,
        name: &str,
        args: &Value,
    ) -> String {
        exec_tool(root, talk, name, args, None, uuid::Uuid::nil()).await
    }

    #[tokio::test]
    async fn exec_tool_read_write_list_and_blocks_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("TEAM.md"), "# team\n").unwrap();

        let wrote = tool(
            root,
            None,
            "write_file",
            &json!({ "path": "src/a.txt", "content": "hello" }),
        )
        .await;
        assert!(wrote.contains("wrote"), "{wrote}");

        let read = tool(root, None, "read_file", &json!({ "path": "src/a.txt" })).await;
        assert_eq!(read, "hello");

        let listing = tool(root, None, "list_dir", &json!({ "path": "." })).await;
        assert!(listing.contains("TEAM.md"), "{listing}");
        assert!(listing.contains("src/"), "{listing}");

        let escape = tool(root, None, "read_file", &json!({ "path": "../secret" })).await;
        assert!(escape.contains("escapes"), "{escape}");

        let team = tool(root, None, "read_team", &json!({})).await;
        assert!(team.contains("# team"), "{team}");

        let unknown = tool(root, None, "explode", &json!({})).await;
        assert!(unknown.contains("unknown tool"), "{unknown}");

        let bash = tool(root, None, "run_bash", &json!({ "command": "echo pong" })).await;
        assert!(bash.contains("pong"), "{bash}");
    }

    #[tokio::test]
    async fn exec_tool_send_and_read_across_teammates() {
        let dir = tempfile::tempdir().unwrap();
        let project = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let a = TeamTalk::from_spawn(dir.path(), project, run, "t1", "backend");
        let b = TeamTalk::from_spawn(dir.path(), project, run, "t2", "frontend");
        let wa = dir.path().join("a");
        let wb = dir.path().join("b");
        std::fs::create_dir_all(&wa).unwrap();
        std::fs::create_dir_all(&wb).unwrap();

        let sent = tool(
            &wa,
            Some(&a),
            "send_message",
            &json!({ "to": "t2", "body": "route is POST /v1/hello" }),
        )
        .await;
        assert!(sent.contains("sent"), "{sent}");

        let inbox = tool(&wb, Some(&b), "read_messages", &json!({})).await;
        assert!(inbox.contains("POST /v1/hello"), "{inbox}");
        assert!(inbox.contains("`t1`"), "{inbox}");
    }

    #[tokio::test]
    async fn exec_tool_request_changes_writes_review() {
        let dir = tempfile::tempdir().unwrap();
        let out = tool(
            dir.path(),
            None,
            "request_changes",
            &json!({ "to": "t1", "reason": "add tests for /health" }),
        )
        .await;
        assert!(out.contains("recorded"), "{out}");
        let req = crate::sandbox::share::read_changes_request(dir.path()).unwrap();
        assert_eq!(req.targets, ["t1"]);
        assert!(dir.path().join("REVIEW.md").exists());
    }

    struct FakeIo;

    #[async_trait::async_trait]
    impl super::super::io::IntegrationInvoker for FakeIo {
        async fn invoke(
            &self,
            _workspace_id: uuid::Uuid,
            provider: String,
            operation: String,
            args: serde_json::Value,
        ) -> super::super::io::IntegrationResult {
            super::super::io::IntegrationResult {
                ok: true,
                summary: format!("{provider}.{operation} ok"),
                data: args,
            }
        }
    }

    #[tokio::test]
    async fn exec_tool_use_integration_goes_through_control_plane() {
        let dir = tempfile::tempdir().unwrap();
        let inv: std::sync::Arc<dyn super::super::io::IntegrationInvoker> =
            std::sync::Arc::new(FakeIo);
        let out = exec_tool(
            dir.path(),
            None,
            "use_integration",
            &json!({
                "provider": "slack",
                "operation": "post_message",
                "args": { "channel": "C1", "text": "hi" }
            }),
            Some(inv),
            uuid::Uuid::nil(),
        )
        .await;
        assert!(out.contains("slack.post_message ok"), "{out}");
        assert!(out.contains("C1"), "{out}");
    }

    #[test]
    fn ship_helpers_find_pr_owner_and_skip_junk() {
        assert_eq!(
            extract_github_pr_url("see https://github.com/OmChillure/test/pull/2 thanks"),
            Some("https://github.com/OmChillure/test/pull/2".into())
        );
        assert_eq!(
            find_owner_repo("branch feature/calculator for OmChillure/test"),
            Some(("OmChillure".into(), "test".into()))
        );
        assert_eq!(
            find_branch("use `feature/calculator` please"),
            "feature/calculator"
        );
        assert!(skip_ship_name("commit_payload.json"));
        assert!(skip_ship_name("ship_all_files_commit.json"));
        assert!(!skip_ship_name("page.tsx"));
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("app")).unwrap();
        std::fs::write(
            work.join("SHIP.md"),
            "PR: https://github.com/OmChillure/test/pull/2\n",
        )
        .unwrap();
        std::fs::write(work.join("commit_payload.json"), "{\"files\":[]}\n").unwrap();
        std::fs::write(
            work.join("app").join("page.tsx"),
            "export default function Page(){}\n",
        )
        .unwrap();
        assert_eq!(
            find_existing_pr_url(dir.path()).as_deref(),
            Some("https://github.com/OmChillure/test/pull/2")
        );
        let files = collect_ship_files(dir.path());
        assert!(files.iter().any(|(p, _)| p == "app/page.tsx"), "{files:?}");
        assert!(
            files.iter().all(|(p, _)| p != "commit_payload.json"),
            "{files:?}"
        );
    }

    #[tokio::test]
    async fn run_ship_reuses_existing_pr_without_io() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(
            dir.path().join("work").join("SHIP.md"),
            "PR: https://github.com/OmChillure/test/pull/2\n",
        )
        .unwrap();
        let code = run_ship(
            dir.path(),
            crate::sandbox::io::IntegrationHandle::none(),
            uuid::Uuid::nil(),
            "ship OmChillure/test",
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        let result = std::fs::read_to_string(dir.path().join("result.md")).unwrap();
        assert!(
            result.contains("https://github.com/OmChillure/test/pull/2"),
            "{result}"
        );
    }

    #[tokio::test]
    async fn agent_job_abort_sets_exit_143() {
        let dir = tempfile::tempdir().unwrap();
        let req = SpawnRequest {
            session_id: uuid::Uuid::new_v4(),
            project_id: uuid::Uuid::new_v4(),
            run_id: uuid::Uuid::new_v4(),
            board_task_id: uuid::Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: dir.path().to_path_buf(),
            env: Default::default(),
            timeout: std::time::Duration::from_secs(30),
            task_title: None,
            task_summary: None,
            task_spec: None,
            persona_name: None,
            persona_md: None,
            agent_backend: Some("grok".into()),
            agent_model: Some("grok-4.5".into()),
            io: crate::sandbox::io::IntegrationHandle::none(),
        };
        let job = AgentJob::start(&req);
        job.abort();
        assert_eq!(job.poll_exit(), Some(143));
    }
}
