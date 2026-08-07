//! Workspace chat tools: fleet sandboxes + control-plane integration proxy.

use std::time::Duration;

use fleet_auth::{AppError, AppResult};
use fleet_db::repos::integrations as integ_repo;
use fleet_integrations::proxy::{
    self, format_context_markdown, operations_for, supported_providers, ProxyCaller, ProxyRequest,
    ProxyResult,
};
use fleet_integrations::{catalog, IntegrationAvailability};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus, MAX_PROJECT_AGENTS,
    MAX_TASKS_PER_PLAN,
};
use fleet_orchestrator::validate;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

pub const MAX_TOOL_ROUNDS: usize = 6;
/// Chat plans use the orchestrator ceiling (1000). Concurrent workers are
/// `FLEET_MAX_AGENTS` (default 16) — extras queue locally, no k8s required.
pub const MAX_CHAT_PLAN_TASKS: usize = MAX_TASKS_PER_PLAN;
/// Initial ticks in the chat request so the first wave starts; background ticker drains the rest.
pub const MAX_CHAT_TICKS: usize = 24;
pub const MAX_CHAT_TICKS_LARGE: usize = 3;

pub fn chat_max_agents() -> i32 {
    std::env::var("FLEET_MAX_AGENTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16)
        .clamp(1, MAX_PROJECT_AGENTS)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatAction {
    pub tool: String,
    pub ok: bool,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_sandboxes",
                "description": "List worker sandboxes (sessions) for this workspace: status, backend, run/task ids.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "List fleet board tasks (title, status, run id). Optional status filter e.g. ready, in_progress, done_success.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "Optional task status filter"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_runs",
                "description": "List fleet work runs (id, status, source).",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_fleet_status",
                "description": "Compact fleet snapshot: open runs, task status counts, active sandboxes.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "launch_agent_task",
                "description": "Create a single-task agent run, multi-tick until the stub sandbox finishes, then destroy it. Prefer launch_agent_plan for multi-agent fan-out. Optional context_from prefetches GitHub/Jira/Linear into the task spec (tokens stay on the control plane).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Short task title"
                        },
                        "spec": {
                            "type": "string",
                            "description": "Task description / acceptance notes for the agent"
                        },
                        "worker_profile": {
                            "type": "string",
                            "description": "Worker profile (default: coding)"
                        },
                        "max_agents": {
                            "type": "integer",
                            "description": "Concurrent sandboxes (default 16, env FLEET_MAX_AGENTS). Extra tasks queue until a slot frees — no k8s required."
                        },
                        "context_from": {
                            "type": "array",
                            "description": "Optional integration proxy fetches to embed (no secrets) into the task spec",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "provider": { "type": "string" },
                                    "operation": { "type": "string" },
                                    "args": { "type": "object" }
                                },
                                "required": ["provider", "operation"]
                            }
                        }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "launch_agent_plan",
                "description": "Multi-agent fan-out: DAG of tasks (deps + parallel). Up to 1000 tasks; only FLEET_MAX_AGENTS run at once (rest queue). Sandboxes are local stub/process/docker — not k8s. Large plans return after the first wave; background ticks drain the queue.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Optional run label"
                        },
                        "max_agents": {
                            "type": "integer",
                            "description": "Concurrent sandboxes (default 16). Remaining ready tasks wait for free slots."
                        },
                        "context_from": {
                            "type": "array",
                            "description": "Shared integration context prefetched into every task spec",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "provider": { "type": "string" },
                                    "operation": { "type": "string" },
                                    "args": { "type": "object" }
                                },
                                "required": ["provider", "operation"]
                            }
                        },
                        "tasks": {
                            "type": "array",
                            "description": "Tasks with optional depends_on (plan_task ids)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "Stable plan task id e.g. t1" },
                                    "title": { "type": "string" },
                                    "spec": { "type": "string" },
                                    "depends_on": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    },
                                    "worker_profile": { "type": "string" },
                                    "priority": { "type": "integer" }
                                },
                                "required": ["id", "title"]
                            }
                        }
                    },
                    "required": ["tasks"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_integrations",
                "description": "List workspace integrations (GitHub, Jira, Slack, Linear, …): catalog, OAuth configured, connected or not, account labels, and what each connection enables. Use when the user asks what tools/platforms are linked.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "connected_only": {
                            "type": "boolean",
                            "description": "If true, only return connected providers"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_integration",
                "description": "Details for one integration provider id (e.g. github, slack, jira, linear): connection status, label, and capability notes. Does not return secrets.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "provider": {
                            "type": "string",
                            "description": "Provider id from the catalog (github, jira, slack, linear, …)"
                        }
                    },
                    "required": ["provider"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "integration_query",
                "description": "Call a connected integration on the CONTROL PLANE (decrypt there; tokens never enter sandboxes). Providers: github, jira, linear, slack, figma, notion, google_drive, teams, discord. Use get_integration for the allowlisted ops. GitHub: list_repos/issues/prs, get_file, list_commits, commit_files, create_pr/issue, add_comment. Jira: search/create/transition issues, comments. Linear: list/create/update issues. Slack: list_channels/users, post_message, history. Figma: get_file/nodes/comments. Notion: search/pages/databases. Drive: list/get/export files (readonly). Teams: chats/teams/channels + send messages. Discord: get_me, list_guilds. Do NOT put tokens in sandbox specs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "provider": {
                            "type": "string",
                            "description": "github | jira | linear | slack | figma | notion | google_drive | teams | discord"
                        },
                        "operation": {
                            "type": "string",
                            "description": "Allowlisted operation for that provider (see get_integration)"
                        },
                        "args": {
                            "type": "object",
                            "description": "Op args. Examples — github commit_files: {owner,repo,branch,message,files:[{path,content}]}; slack post_message: {channel,text}; jira create_issue: {project,summary}; notion search: {query}; teams send_chat_message: {chat_id,text}"
                        }
                    },
                    "required": ["provider", "operation"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "github_commit_files",
                "description": "CONTROL PLANE: commit one or more text files to a GitHub branch using the workspace OAuth token (never sent to sandboxes). Creates the branch from default if missing. Use for scaffolding (README, package.json, etc.). Max 40 files, 512KB each.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "owner": { "type": "string" },
                        "repo": { "type": "string", "description": "Repo name or owner/name" },
                        "branch": {
                            "type": "string",
                            "description": "Target branch (default agent-fleet/changes)"
                        },
                        "message": { "type": "string", "description": "Commit message" },
                        "base_branch": {
                            "type": "string",
                            "description": "Branch to fork from if target is new"
                        },
                        "files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string" },
                                    "content": { "type": "string" }
                                },
                                "required": ["path", "content"]
                            }
                        }
                    },
                    "required": ["owner", "repo", "files"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "github_create_pr",
                "description": "CONTROL PLANE: open a GitHub pull request using workspace OAuth. Call after github_commit_files. Returns PR number and html_url.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "owner": { "type": "string" },
                        "repo": { "type": "string" },
                        "title": { "type": "string" },
                        "head": {
                            "type": "string",
                            "description": "Branch with commits (e.g. agent-fleet/changes)"
                        },
                        "base": {
                            "type": "string",
                            "description": "Target branch (default repo default)"
                        },
                        "body": { "type": "string" },
                        "draft": { "type": "boolean" }
                    },
                    "required": ["owner", "repo", "title", "head"],
                    "additionalProperties": false
                }
            }
        }
    ])
}

pub fn system_prompt_with_tools() -> &'static str {
    "You are the Agent Fleet assistant — a helpful AI embedded in a developer platform that runs fleets of sandboxed coding agents and connects to external developer tools.\n\n\
You have tools for:\n\
**Fleet / sandboxes** (ephemeral: spawn → work → destroy; NEVER put OAuth tokens or git credentials in sandbox specs)\n\
- list_sandboxes / list_tasks / list_runs / get_fleet_status\n\
- launch_agent_task / launch_agent_plan: local sandboxes (stub/process/docker). Plans up to 1000 tasks; only FLEET_MAX_AGENTS run at once, the rest queue. Use ```bash in specs for commands.\n\
**Integrations — CONTROL PLANE only** (tokens decrypt on the server, never in sandboxes)\n\
- list_integrations / get_integration (shows connected apps + allowlisted ops)\n\
- integration_query: github, jira, linear, slack, figma, notion, google_drive, teams, discord\n\
- github_commit_files: write/scaffold files + commit on a branch (use for Next.js stubs, README, etc.)\n\
- github_create_pr: open a PR after commits\n\
GitHub write flow: github_commit_files → github_create_pr. Report commit_sha and PR html_url to the user.\n\
Slack HITL: post_message / get_channel_history. Teams: send_chat_message / send_channel_message.\n\
Do NOT claim a git push or external write succeeded unless a control-plane tool returned ok.\n\
If a provider is not connected, tell the user to open Workspace → Integrations and Connect.\n\
Be concise. Summarize tool results with links/ids."
}

pub async fn system_prompt_for_workspace(state: &AppState, workspace_id: Uuid) -> String {
    let mut base = system_prompt_with_tools().to_string();
    match build_integrations_snapshot(state, workspace_id, false).await {
        Ok((content, _)) => {
            if let Some(arr) = content.get("integrations").and_then(|v| v.as_array()) {
                let connected: Vec<String> = arr
                    .iter()
                    .filter(|i| i.get("connected").and_then(|c| c.as_bool()) == Some(true))
                    .filter_map(|i| {
                        let id = i.get("id")?.as_str()?;
                        let label = i
                            .get("account_label")
                            .and_then(|l| l.as_str())
                            .filter(|s| !s.is_empty());
                        Some(match label {
                            Some(l) => format!("{id} ({l})"),
                            None => id.to_string(),
                        })
                    })
                    .collect();
                if connected.is_empty() {
                    base.push_str(
                        "\n\nWorkspace integrations: none connected yet. User can connect them under Integrations.",
                    );
                } else {
                    base.push_str(&format!(
                        "\n\nWorkspace integrations currently connected: {}.",
                        connected.join(", ")
                    ));
                }
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "integrations snapshot for system prompt failed");
        }
    }
    base
}

#[derive(Debug, Deserialize)]
struct LaunchArgs {
    title: String,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    worker_profile: Option<String>,
    #[serde(default)]
    max_agents: Option<i32>,
    #[serde(default)]
    context_from: Option<Vec<ContextFromArg>>,
}

#[derive(Debug, Deserialize, Clone)]
struct ContextFromArg {
    provider: String,
    operation: String,
    #[serde(default)]
    args: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct PlanTaskArg {
    id: String,
    title: String,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
    #[serde(default)]
    worker_profile: Option<String>,
    #[serde(default)]
    priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct LaunchPlanArgs {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    max_agents: Option<i32>,
    #[serde(default)]
    context_from: Option<Vec<ContextFromArg>>,
    tasks: Vec<PlanTaskArg>,
}

#[derive(Debug, Deserialize)]
struct ListTasksArgs {
    #[serde(default)]
    status: Option<String>,
}

pub async fn execute_tool(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    name: &str,
    arguments: &Value,
) -> (Value, ChatAction) {
    let result = match name {
        "list_sandboxes" => list_sandboxes(state, workspace_id).await,
        "list_tasks" => {
            let status = arguments
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let status = status.or_else(|| {
                serde_json::from_value::<ListTasksArgs>(arguments.clone())
                    .ok()
                    .and_then(|a| a.status)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
            list_tasks(state, workspace_id, status.as_deref()).await
        }
        "list_runs" => list_runs(state, workspace_id).await,
        "get_fleet_status" => get_fleet_status(state, workspace_id).await,
        "launch_agent_task" => match parse_launch_args(arguments) {
            Ok(args) => launch_agent_task(state, workspace_id, user_id, args).await,
            Err(e) => Err(AppError::bad_request(e)),
        },
        "launch_agent_plan" => match parse_launch_plan_args(arguments) {
            Ok(args) => launch_agent_plan(state, workspace_id, user_id, args).await,
            Err(e) => Err(AppError::bad_request(e)),
        },
        "list_integrations" => {
            let connected_only = arguments
                .get("connected_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            build_integrations_snapshot(state, workspace_id, connected_only).await
        }
        "get_integration" => {
            let provider = arguments
                .get("provider")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::bad_request("provider is required".to_string()));
            match provider {
                Ok(p) => get_integration(state, workspace_id, &p).await,
                Err(e) => Err(e),
            }
        }
        "integration_query" => integration_query(state, workspace_id, user_id, arguments).await,
        "github_commit_files" => {
            github_tool(state, workspace_id, user_id, "commit_files", arguments).await
        }
        "github_create_pr" => {
            github_tool(state, workspace_id, user_id, "create_pr", arguments).await
        }
        other => Err(AppError::bad_request(format!("unknown tool `{other}`"))),
    };

    match result {
        Ok((content, action)) => (content, action),
        Err(e) => {
            let msg = e.to_string();
            (
                json!({ "error": msg }),
                ChatAction {
                    tool: name.to_string(),
                    ok: false,
                    summary: msg,
                    run_id: None,
                    sandbox_ids: None,
                    detail: None,
                },
            )
        }
    }
}

fn parse_json_args(arguments: &Value) -> Result<Value, String> {
    if let Some(s) = arguments.as_str() {
        serde_json::from_str(s).map_err(|e| format!("invalid tool args JSON: {e}"))
    } else {
        Ok(arguments.clone())
    }
}

fn parse_launch_args(arguments: &Value) -> Result<LaunchArgs, String> {
    let obj = parse_json_args(arguments)?;
    let args: LaunchArgs =
        serde_json::from_value(obj).map_err(|e| format!("invalid launch_agent_task args: {e}"))?;
    let title = args.title.trim().to_string();
    if title.is_empty() {
        return Err("title must not be empty".into());
    }
    if title.len() > 200 {
        return Err("title too long (max 200 chars)".into());
    }
    Ok(LaunchArgs {
        title,
        spec: args.spec,
        worker_profile: args.worker_profile,
        max_agents: args.max_agents,
        context_from: args.context_from,
    })
}

fn parse_launch_plan_args(arguments: &Value) -> Result<LaunchPlanArgs, String> {
    let obj = parse_json_args(arguments)?;
    let args: LaunchPlanArgs =
        serde_json::from_value(obj).map_err(|e| format!("invalid launch_agent_plan args: {e}"))?;
    if args.tasks.is_empty() {
        return Err("tasks must not be empty".into());
    }
    if args.tasks.len() > MAX_CHAT_PLAN_TASKS {
        return Err(format!(
            "too many tasks (max {MAX_CHAT_PLAN_TASKS} from chat)"
        ));
    }
    for t in &args.tasks {
        if t.id.trim().is_empty() {
            return Err("each task needs a non-empty id".into());
        }
        if t.title.trim().is_empty() {
            return Err(format!("task `{}` needs a non-empty title", t.id));
        }
        if t.title.len() > 200 {
            return Err(format!("task `{}` title too long", t.id));
        }
    }
    Ok(args)
}

pub fn build_chat_plan(
    workspace_id: Uuid,
    title: &str,
    spec: &str,
    worker_profile: &str,
) -> WorkPlanBody {
    build_multi_task_plan(
        workspace_id,
        None,
        vec![PlanTask {
            plan_task_id: "t1".into(),
            title: title.to_string(),
            summary: spec.chars().take(280).collect(),
            spec_md: spec.to_string(),
            depends_on: vec![],
            worker_profile: worker_profile.to_string(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        }],
    )
}

pub fn build_multi_task_plan(
    workspace_id: Uuid,
    max_agents_override: Option<i32>,
    tasks: Vec<PlanTask>,
) -> WorkPlanBody {
    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    WorkPlanBody {
        plan_id,
        project_id: workspace_id,
        run_id,
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "chat".into(),
        tasks,
        policy: PlanPolicy {
            max_agents_override,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
        },
    }
}

async fn ensure_project(state: &AppState, workspace_id: Uuid, max_agents: i32) -> AppResult<()> {
    let max = max_agents.clamp(1, chat_max_agents());
    state
        .orch
        .store
        .ensure_project(workspace_id, "workspace fleet", max)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, error = %e, "chat tool ensure_project failed");
            AppError::internal(e.to_string())
        })?;
    if let Err(e) = state.orch.store.set_max_agents(workspace_id, max).await {
        tracing::warn!(%workspace_id, error = %e, "set_max_agents failed (continuing)");
    }
    Ok(())
}

async fn prefetch_context(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    context_from: &[ContextFromArg],
) -> Vec<ProxyResult> {
    let mut out = Vec::new();
    for item in context_from.iter().take(5) {
        let provider = item.provider.trim().to_lowercase();
        let operation = item.operation.trim().to_lowercase();
        let args = item.args.clone().unwrap_or_else(|| json!({}));
        let result = proxy::invoke(
            &state.db,
            &state.integrations.credentials_key,
            ProxyRequest {
                workspace_id,
                provider,
                operation,
                args,
                caller: ProxyCaller::Chat { user_id },
            },
        )
        .await;
        out.push(result);
    }
    out
}

async fn integration_query(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let obj = parse_json_args(arguments).map_err(AppError::bad_request)?;
    let provider = obj
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("provider is required"))?;
    let operation = obj
        .get("operation")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("operation is required"))?;
    let args = obj.get("args").cloned().unwrap_or_else(|| json!({}));

    proxy_tool_result(
        state,
        workspace_id,
        user_id,
        "integration_query",
        &provider,
        &operation,
        args,
    )
    .await
}

async fn github_tool(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    operation: &str,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let args = parse_json_args(arguments).map_err(AppError::bad_request)?;
    let tool_name = if operation == "create_pr" {
        "github_create_pr"
    } else {
        "github_commit_files"
    };
    proxy_tool_result(
        state,
        workspace_id,
        user_id,
        tool_name,
        "github",
        operation,
        args,
    )
    .await
}

async fn proxy_tool_result(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    tool_name: &str,
    provider: &str,
    operation: &str,
    args: Value,
) -> AppResult<(Value, ChatAction)> {
    let result = proxy::invoke(
        &state.db,
        &state.integrations.credentials_key,
        ProxyRequest {
            workspace_id,
            provider: provider.to_string(),
            operation: operation.to_string(),
            args,
            caller: ProxyCaller::Chat { user_id },
        },
    )
    .await;

    let content = json!({
        "ok": result.ok,
        "provider": result.provider,
        "operation": result.operation,
        "summary": result.summary,
        "data": result.data,
        "error_code": result.error_code,
        "provider_status": result.provider_status,
        "security": {
            "tokens_in_sandbox": false,
            "decrypt_on": "control_plane",
        },
        "hint": "Tokens stay on the control plane; this payload is sanitized.",
        "supported_providers": supported_providers(),
        "operations": operations_for(provider),
    });

    Ok((
        content.clone(),
        ChatAction {
            tool: tool_name.into(),
            ok: result.ok,
            summary: result.summary.clone(),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn tick_until_run_settled(
    state: &AppState,
    workspace_id: Uuid,
    run_id: Uuid,
    max_ticks: usize,
) -> AppResult<(Vec<Value>, bool)> {
    let mut ticks = Vec::new();
    let mut terminal = false;
    let stub_sleep = std::env::var("STUB_DURATION_S")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    for i in 0..max_ticks {
        if i > 0 {
            let wait = if stub_sleep > 0 {
                Duration::from_secs(stub_sleep.min(3))
            } else {
                Duration::from_millis(20)
            };
            tokio::time::sleep(wait).await;
        }

        let report = state
            .orch
            .tick
            .tick_project(workspace_id)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("lock") {
                    AppError::conflict(msg)
                } else {
                    tracing::error!(%workspace_id, error = %msg, "chat multi-tick failed");
                    AppError::internal(msg)
                }
            })?;

        ticks.push(json!({
            "tick": i + 1,
            "tick_id": report.tick_id,
            "plans_materialized": report.plans_materialized,
            "tasks_materialized": report.tasks_materialized,
            "spawned": report.spawned,
            "spawn_failed": report.spawn_failed,
            "completed_via_runtime": report.completed_via_runtime,
            "reaped": report.reaped,
            "runs_terminated": report.runs_terminated,
        }));

        if let Ok(Some(run)) = state.orch.store.get_run(workspace_id, run_id).await {
            let st = run.status.as_str();
            if matches!(st, "completed" | "failed" | "cancelled") {
                terminal = true;
                break;
            }
        }

        let tasks = state
            .orch
            .store
            .list_tasks(workspace_id, Some(run_id), None)
            .await
            .unwrap_or_default();
        let active = tasks.iter().any(|t| {
            matches!(
                t.status.as_str(),
                "queued" | "ready" | "assigned" | "in_progress" | "retry_wait"
            )
        });
        if !active && i > 0 && ticks.len() >= 2 {
            break;
        }
    }

    Ok((ticks, terminal))
}

async fn list_sandboxes(state: &AppState, workspace_id: Uuid) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id, 3).await?;
    let sessions = state
        .orch
        .store
        .list_sessions_for_project(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let sandboxes: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "run_id": s.run_id,
                "board_task_id": s.board_task_id,
                "status": s.status,
                "runtime_backend": s.runtime_backend,
                "worker_profile": s.worker_profile,
                "progress_pct": s.progress_pct,
                "exit_reason": s.exit_reason,
                "started_at": s.started_at,
                "exited_at": s.exited_at,
            })
        })
        .collect();
    let n = sandboxes.len();
    let ids: Vec<String> = sessions.iter().map(|s| s.id.to_string()).collect();
    let content = json!({ "count": n, "sandboxes": sandboxes });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_sandboxes".into(),
            ok: true,
            summary: format!("{n} sandbox(es)"),
            run_id: None,
            sandbox_ids: if ids.is_empty() { None } else { Some(ids) },
            detail: Some(content),
        },
    ))
}

async fn list_tasks(
    state: &AppState,
    workspace_id: Uuid,
    status: Option<&str>,
) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id, 3).await?;
    let tasks = state
        .orch
        .store
        .list_tasks(workspace_id, None, status)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let items: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "run_id": t.run_id,
                "plan_task_id": t.plan_task_id,
                "title": t.title,
                "status": t.status,
                "priority": t.priority,
                "progress_pct": t.progress_pct,
            })
        })
        .collect();
    let n = items.len();
    let content = json!({ "count": n, "tasks": items });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_tasks".into(),
            ok: true,
            summary: format!("{n} task(s)"),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn list_runs(state: &AppState, workspace_id: Uuid) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id, 3).await?;
    let runs = state
        .orch
        .store
        .list_runs(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let items: Vec<Value> = runs
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "status": r.status,
                "source": r.source,
                "created_at": r.created_at,
            })
        })
        .collect();
    let n = items.len();
    let content = json!({ "count": n, "runs": items });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_runs".into(),
            ok: true,
            summary: format!("{n} run(s)"),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn get_fleet_status(state: &AppState, workspace_id: Uuid) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id, 3).await?;
    let runs = state
        .orch
        .store
        .list_runs(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let tasks = state
        .orch
        .store
        .list_tasks(workspace_id, None, None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let sessions = state
        .orch
        .store
        .list_sessions_for_project(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let cfg = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    let mut task_counts: serde_json::Map<String, Value> = serde_json::Map::new();
    for t in &tasks {
        let entry = task_counts.entry(t.status.clone()).or_insert(json!(0));
        if let Some(n) = entry.as_u64() {
            *entry = json!(n + 1);
        }
    }
    let open_runs = runs
        .iter()
        .filter(|r| {
            !matches!(
                r.status.as_str(),
                "completed" | "failed" | "cancelled"
            )
        })
        .count();
    let active_sandboxes = sessions
        .iter()
        .filter(|s| {
            matches!(
                s.status.as_str(),
                "spawning" | "healthy" | "stuck" | "killing"
            )
        })
        .count();

    let content = json!({
        "sandbox_backend": cfg.sandbox_backend,
        "max_agents": cfg.max_agents,
        "open_runs": open_runs,
        "total_runs": runs.len(),
        "task_counts": task_counts,
        "total_tasks": tasks.len(),
        "active_sandboxes": active_sandboxes,
        "total_sandboxes": sessions.len(),
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "get_fleet_status".into(),
            ok: true,
            summary: format!(
                "{open_runs} open run(s), {active_sandboxes} active sandbox(es)"
            ),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn launch_agent_task(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: LaunchArgs,
) -> AppResult<(Value, ChatAction)> {
    let max_agents = args.max_agents.unwrap_or_else(chat_max_agents).clamp(1, chat_max_agents());
    ensure_project(state, workspace_id, max_agents).await?;

    let profile = args
        .worker_profile
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("coding");
    let mut spec = args
        .spec
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    if spec.len() > 32_000 {
        return Err(AppError::bad_request("spec too long (max 32k characters)"));
    }

    let ctx = if let Some(ref cf) = args.context_from {
        prefetch_context(state, workspace_id, user_id, cf).await
    } else {
        Vec::new()
    };
    if !ctx.is_empty() {
        spec.push_str(&format_context_markdown(&ctx));
    }

    let plan = build_chat_plan(workspace_id, &args.title, &spec, profile);
    accept_and_drive_plan(state, workspace_id, plan, "launch_agent_task", &args.title, ctx).await
}

async fn launch_agent_plan(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: LaunchPlanArgs,
) -> AppResult<(Value, ChatAction)> {
    let max_agents = args.max_agents.unwrap_or_else(chat_max_agents).clamp(1, chat_max_agents());
    ensure_project(state, workspace_id, max_agents).await?;

    let ctx = if let Some(ref cf) = args.context_from {
        prefetch_context(state, workspace_id, user_id, cf).await
    } else {
        Vec::new()
    };
    let ctx_md = if ctx.is_empty() {
        String::new()
    } else {
        format_context_markdown(&ctx)
    };

    let tasks: Vec<PlanTask> = args
        .tasks
        .iter()
        .map(|t| {
            let mut spec = t
                .spec
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            if !ctx_md.is_empty() {
                spec.push_str(&ctx_md);
            }
            if spec.len() > 32_000 {
                spec.truncate(32_000);
            }
            PlanTask {
                plan_task_id: t.id.trim().to_string(),
                title: t.title.trim().to_string(),
                summary: spec.chars().take(280).collect(),
                spec_md: spec,
                depends_on: t
                    .depends_on
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                worker_profile: t
                    .worker_profile
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("coding")
                    .to_string(),
                acceptance: vec![],
                priority: t.priority.unwrap_or(100),
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: Some(1),
            }
        })
        .collect();

    let plan = build_multi_task_plan(workspace_id, Some(max_agents), tasks);
    if let Err(e) = validate::validate_plan(&plan) {
        return Err(AppError::bad_request(e.to_string()));
    }

    let label = args
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("multi-agent plan");
    accept_and_drive_plan(
        state,
        workspace_id,
        plan,
        "launch_agent_plan",
        label,
        ctx,
    )
    .await
}

async fn accept_and_drive_plan(
    state: &AppState,
    workspace_id: Uuid,
    plan: WorkPlanBody,
    tool_name: &str,
    label: &str,
    context_results: Vec<ProxyResult>,
) -> AppResult<(Value, ChatAction)> {
    let run_id = plan.run_id;
    let task_count = plan.tasks.len();

    let (run, plan_row) = state
        .orch
        .store
        .insert_accepted_plan(workspace_id, plan)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, error = %e, "chat launch insert_accepted_plan failed");
            AppError::bad_request(e.to_string())
        })?;

    // Small plans: wait until terminal in this request.
    // Large plans: start the first wave; background ticker (fleet-server) drains the queue.
    let max_ticks = if task_count <= 8 {
        MAX_CHAT_TICKS
    } else {
        MAX_CHAT_TICKS_LARGE
    };
    let (ticks, terminal) = tick_until_run_settled(state, workspace_id, run_id, max_ticks).await?;

    let sessions = state
        .orch
        .store
        .list_sessions_for_project(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let sandbox_ids: Vec<String> = sessions
        .iter()
        .filter(|s| s.run_id == run_id)
        .map(|s| s.id.to_string())
        .collect();
    let destroyed_n = sessions
        .iter()
        .filter(|s| s.run_id == run_id && matches!(s.status.as_str(), "exited" | "failed_spawn"))
        .count();

    let tasks = state
        .orch
        .store
        .list_tasks(workspace_id, Some(run_id), None)
        .await
        .unwrap_or_default();
    let task_statuses: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "plan_task_id": t.plan_task_id,
                "title": t.title,
                "status": t.status,
            })
        })
        .collect();

    let final_run = state
        .orch
        .store
        .get_run(workspace_id, run_id)
        .await
        .ok()
        .flatten();
    let run_status = final_run
        .as_ref()
        .map(|r| r.status.clone())
        .unwrap_or_else(|| run.status.clone());

    let context_summaries: Vec<Value> = context_results
        .iter()
        .map(|r| {
            json!({
                "provider": r.provider,
                "operation": r.operation,
                "ok": r.ok,
                "summary": r.summary,
            })
        })
        .collect();

    let hint = if terminal {
        "Sandboxes are ephemeral. Integration tokens stay on the control plane."
    } else {
        "Run is draining in the background (local queue, no k8s). Check Fleet / Sandboxes / get_fleet_status."
    };
    let content = json!({
        "run_id": run_id,
        "plan_id": plan_row.id,
        "run_status": run_status,
        "title": label,
        "task_count": task_count,
        "tasks": task_statuses,
        "ticks": ticks,
        "run_terminal": terminal,
        "sandbox_ids": sandbox_ids,
        "sandboxes_destroyed": destroyed_n,
        "integration_context": context_summaries,
        "security": {
            "tokens_in_sandbox": false,
            "decrypt_on": "control_plane",
            "lifecycle": "spawn → work → destroy → wipe workdir",
        },
        "max_concurrent_agents": chat_max_agents(),
        "queued": !terminal && task_count > chat_max_agents() as usize,
        "hint": hint,
    });

    let summary = format!(
        "Launched «{label}» · {task_count} task(s) · run {} · {} sandbox(es) · terminal={terminal} · destroyed≈{destroyed_n}",
        short_id(&run_id.to_string()),
        sandbox_ids.len(),
    );

    Ok((
        content.clone(),
        ChatAction {
            tool: tool_name.into(),
            ok: true,
            summary,
            run_id: Some(run_id.to_string()),
            sandbox_ids: if sandbox_ids.is_empty() {
                None
            } else {
                Some(sandbox_ids)
            },
            detail: Some(content),
        },
    ))
}

fn short_id(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

fn provider_capabilities(provider: &str) -> &'static str {
    match provider {
        "github" => {
            "list_repos, list_issues, get_issue, list_prs, get_pr, get_file, list_commits, put_file, commit_files, create_branch, create_pr, create_issue, add_comment"
        }
        "jira" => {
            "search_issues, get_issue, list_projects, create_issue, add_comment, list_transitions, transition_issue"
        }
        "linear" => {
            "list_issues, get_issue, list_teams, create_issue, update_issue, add_comment"
        }
        "slack" => "list_channels, list_users, post_message, get_channel_history",
        "figma" => {
            "get_me, get_file, get_file_nodes, list_comments, list_team_projects, get_project_files"
        }
        "notion" => {
            "search, get_page, get_database, query_database, get_block_children, create_page, append_blocks"
        }
        "google_drive" => "list_files, get_file, export_file (readonly)",
        "teams" => {
            "get_me, list_chats, list_teams, list_channels, list_chat_messages, send_chat_message, send_channel_message"
        }
        "discord" => "get_me, list_guilds (user OAuth; no channel posts)",
        _ => "OAuth connect only — no proxy tools yet.",
    }
}

async fn build_integrations_snapshot(
    state: &AppState,
    workspace_id: Uuid,
    connected_only: bool,
) -> AppResult<(Value, ChatAction)> {
    let connections = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let by_provider: std::collections::HashMap<&str, &fleet_db::IntegrationConnection> =
        connections
            .iter()
            .map(|c| (c.provider.as_str(), c))
            .collect();

    let mut items = Vec::new();
    for view in catalog() {
        let conn = by_provider.get(view.id.as_str()).copied();
        let connected = conn.is_some_and(|c| c.status == "connected");
        if connected_only && !connected {
            continue;
        }
        let oauth_configured = state.integrations.is_oauth_configured(&view.id);
        let availability = match view.availability {
            IntegrationAvailability::Available => "available",
            IntegrationAvailability::ComingSoon => "coming_soon",
        };
        items.push(json!({
            "id": view.id,
            "name": view.name,
            "category": view.category_label,
            "description": view.description,
            "availability": availability,
            "oauth_configured": oauth_configured,
            "connected": connected,
            "status": conn.map(|c| c.status.as_str()).unwrap_or("disconnected"),
            "account_label": conn.and_then(|c| c.account_label.clone()),
            "last_error": conn.and_then(|c| c.last_error.clone()),
            "capabilities": provider_capabilities(&view.id),
        }));
    }

    let connected_n = items
        .iter()
        .filter(|i| i.get("connected").and_then(|c| c.as_bool()) == Some(true))
        .count();
    let content = json!({
        "count": items.len(),
        "connected_count": connected_n,
        "integrations": items,
        "hint": "Tokens stay in the control plane. Connect more under Workspace → Integrations.",
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_integrations".into(),
            ok: true,
            summary: format!("{connected_n} connected · {} listed", items.len()),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn get_integration(
    state: &AppState,
    workspace_id: Uuid,
    provider: &str,
) -> AppResult<(Value, ChatAction)> {
    let view = catalog()
        .into_iter()
        .find(|v| v.id == provider)
        .ok_or_else(|| {
            AppError::bad_request(format!(
                "unknown provider `{provider}` — try list_integrations"
            ))
        })?;
    let conn = integ_repo::list_all_for_workspace(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .into_iter()
        .find(|c| c.provider == provider);
    let connected = conn.as_ref().is_some_and(|c| c.status == "connected");
    let oauth_configured = state.integrations.is_oauth_configured(provider);
    let content = json!({
        "id": view.id,
        "name": view.name,
        "category": view.category_label,
        "description": view.description,
        "docs_url": view.docs_url,
        "oauth_configured": oauth_configured,
        "connected": connected,
        "status": conn.as_ref().map(|c| c.status.as_str()).unwrap_or("disconnected"),
        "account_label": conn.as_ref().and_then(|c| c.account_label.clone()),
        "last_error": conn.as_ref().and_then(|c| c.last_error.clone()),
        "connected_at": conn.as_ref().map(|c| c.connected_at),
        "capabilities": provider_capabilities(provider),
        "connect_hint": if connected {
            "Already connected for this workspace."
        } else if oauth_configured {
            "Not connected — user can Connect on the Integrations page."
        } else {
            "OAuth app not configured on the server (missing CLIENT_ID/SECRET)."
        },
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "get_integration".into(),
            ok: true,
            summary: format!(
                "{} · {}",
                view.name,
                if connected { "connected" } else { "not connected" }
            ),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_chat_plan_sets_chat_source_and_single_task() {
        let ws = Uuid::new_v4();
        let plan = build_chat_plan(ws, "Fix flaky tests", "Investigate CI flake", "coding");
        assert_eq!(plan.project_id, ws);
        assert_eq!(plan.source, "chat");
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].title, "Fix flaky tests");
        assert_eq!(plan.tasks[0].worker_profile, "coding");
        assert!(plan.tasks[0].depends_on.is_empty());
    }

    #[test]
    fn parse_launch_args_rejects_empty_title() {
        let v = json!({ "title": "  " });
        assert!(parse_launch_args(&v).is_err());
    }

    #[test]
    fn parse_launch_args_from_string() {
        let v = Value::String(r#"{"title":"Do thing","spec":"details"}"#.into());
        let a = parse_launch_args(&v).unwrap();
        assert_eq!(a.title, "Do thing");
        assert_eq!(a.spec.as_deref(), Some("details"));
    }

    #[test]
    fn chat_plan_cap_matches_orchestrator() {
        assert_eq!(MAX_CHAT_PLAN_TASKS, MAX_TASKS_PER_PLAN);
        assert!(MAX_CHAT_PLAN_TASKS >= 1000);
    }

    #[test]
    fn tool_definitions_include_launch_and_integrations() {
        let tools = tool_definitions();
        let arr = tools.as_array().unwrap();
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"launch_agent_task"));
        assert!(names.contains(&"launch_agent_plan"));
        assert!(names.contains(&"integration_query"));
        assert!(names.contains(&"github_commit_files"));
        assert!(names.contains(&"github_create_pr"));
        assert!(names.contains(&"list_sandboxes"));
        assert!(names.contains(&"list_integrations"));
        assert!(names.contains(&"get_integration"));
    }

    #[test]
    fn multi_task_plan_validates() {
        let plan = build_multi_task_plan(
            Uuid::nil(),
            Some(3),
            vec![
                PlanTask {
                    plan_task_id: "root".into(),
                    title: "Root".into(),
                    summary: String::new(),
                    spec_md: String::new(),
                    depends_on: vec![],
                    worker_profile: "coding".into(),
                    acceptance: vec![],
                    priority: 100,
                    external_ref: None,
                    timeout_s: Some(60),
                    max_retries: Some(0),
                },
                PlanTask {
                    plan_task_id: "left".into(),
                    title: "Left".into(),
                    summary: String::new(),
                    spec_md: String::new(),
                    depends_on: vec!["root".into()],
                    worker_profile: "coding".into(),
                    acceptance: vec![],
                    priority: 50,
                    external_ref: None,
                    timeout_s: Some(60),
                    max_retries: Some(0),
                },
            ],
        );
        let mut plan = plan;
        plan.plan_id = Uuid::new_v4();
        plan.project_id = Uuid::new_v4();
        plan.run_id = Uuid::new_v4();
        assert!(validate::validate_plan(&plan).is_ok());
    }
}
