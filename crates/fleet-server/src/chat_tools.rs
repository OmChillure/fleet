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

pub const MAX_TOOL_ROUNDS: usize = 8;
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
                "description": "Fleet snapshot: open runs, sandboxes, max_agents, fleet_llm, effective_backend. Integrations are optional.",
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
                "description": "Create a single-task agent run, multi-tick until the stub sandbox finishes, then destroy it. Prefer launch_agent_plan for multi-agent fan-out. Optional context_from prefetches a connected integration into the task spec (tokens stay on the control plane).",
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
                            "description": "Persona slug from list_agent_personas (default generalist). Each task can be a different specialist."
                        },
                        "max_agents": {
                            "type": "integer",
                            "description": "Optional override of workspace Settings → concurrent agents. Default is the saved setting."
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
                "description": "Launch a DAG of specialists for work that needs sandboxes. You choose the tasks, worker_profile, depends_on, and outputs from the user's request — do not assume coding or a PR. Workers use THIS chat's provider+model and use_integration for any connected app. Put fetched context in each spec. outputs = write-backs when the run ends (any provider+operation).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Optional run label"
                        },
                        "max_agents": {
                            "type": "integer",
                            "description": "Optional override of workspace Settings → concurrent agents."
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
                        "outputs": {
                            "type": "array",
                            "description": "Write-backs when the run finishes. Any connected provider + allowlisted operation. Example: {provider:\"notion\", operation:\"append_blocks\", args:{page_id:\"…\", text:\"{{summary}}\"}}.",
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
                "name": "configure_fleet",
                "description": "Enable or change workspace fleet workers: concurrent agents + local Fleet LLM (Ollama). Use this yourself when effective_backend is null so workers can use an LLM. Do not ask the user to open Settings first. Grok workers stay env-only (cannot set grok here).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "max_agents": {
                            "type": "integer",
                            "description": "Concurrent sandboxes (1–256)"
                        },
                        "fleet_llm": {
                            "type": "string",
                            "description": "off or ollama"
                        },
                        "fleet_llm_model": {
                            "type": "string",
                            "description": "Ollama tag e.g. llama3.2 or qwen2.5-coder"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_agent_personas",
                "description": "List specialist agent personas to use as worker_profile on launch_agent_task / launch_agent_plan. Any roster slug works (engineering, marketing, finance, healthcare, …). Default if omitted: generalist. Optional shortcuts: coding, frontend, reviewer, testing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "division": {
                            "type": "string",
                            "description": "Optional division filter e.g. engineering, testing, product, design"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_team_chat",
                "description": "Read the live team chat for a fleet run — what the specialist agents said to each other. Defaults to the latest open run.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "run_id": {
                            "type": "string",
                            "description": "Optional run UUID. Omit to use the latest open run."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max messages to return (default 40)"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "send_team_message",
                "description": "Send a message into a live fleet run as the human. Agents see it on their next thinking round. to='*' broadcasts; or use a plan task id (t1).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "body": { "type": "string", "description": "Message text" },
                        "to": {
                            "type": "string",
                            "description": "Teammate plan task id, or * / team (default *)"
                        },
                        "run_id": {
                            "type": "string",
                            "description": "Optional run UUID. Omit to use the latest open run."
                        }
                    },
                    "required": ["body"],
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
                "description": "Read or write a connected app on the CONTROL PLANE (tokens never enter sandboxes). The system prompt lists every provider and allowlisted operation. Use those. If not_connected, say so and still do the rest of the work.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "provider": {
                            "type": "string",
                            "description": "github | gitlab | bitbucket | jira | linear | slack | figma | notion | confluence | google_drive | teams | discord | trello | asana"
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
                "name": "start_case",
                "description": "Start a durable CASE: backend owns status/stage/wake schedule; policy_md is natural language. Wakes in the background until complete/fail/cancel. Integrations optional later. Prefer this for long-running watch→decide→act flows (not one-shot agent plans).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Short case title" },
                        "policy_md": {
                            "type": "string",
                            "description": "Natural-language policy: when to wait, what counts as done, what to do on new facts"
                        },
                        "kind": {
                            "type": "string",
                            "description": "Optional kind label (generic, outreach, ticket, …)"
                        },
                        "stage": {
                            "type": "string",
                            "description": "Optional starting stage string (default start)"
                        },
                        "wake_in_s": {
                            "type": "integer",
                            "description": "Seconds until first wake (default 0 = soon)"
                        },
                        "context": {
                            "type": "object",
                            "description": "Optional initial JSON facts (no secrets)"
                        }
                    },
                    "required": ["title", "policy_md"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_cases",
                "description": "List durable cases for this workspace (status, stage, next_wake).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "Optional filter: open, waiting, deciding, acting, completed, failed, cancelled"
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_case",
                "description": "Get one case with recent events (stage, context, decisions).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "case_id": { "type": "string", "description": "Case UUID" }
                    },
                    "required": ["case_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "observe_case",
                "description": "Inject a human/external fact into a case (e.g. they replied / confirmed a time) and wake the backend engine.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "case_id": { "type": "string" },
                        "note": { "type": "string", "description": "Observation text" },
                        "wake": {
                            "type": "boolean",
                            "description": "Wake soon (default true)"
                        }
                    },
                    "required": ["case_id", "note"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cancel_case",
                "description": "Cancel a durable case (terminal).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "case_id": { "type": "string" }
                    },
                    "required": ["case_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "monitor_tickets",
                "description": "Start or update the workspace ticket inbox. Polls every connected integration (GitHub, GitLab, Bitbucket, Jira, Linear, Trello, Asana, Slack, Teams, Notion, Confluence). Tokens stay on the control plane.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean" },
                        "providers": { "type": "array", "items": { "type": "string" } },
                        "github_repos": { "type": "array", "items": { "type": "string" } },
                        "jira_jql": { "type": "string" },
                        "slack_channels": { "type": "array", "items": { "type": "string" } },
                        "auto_launch": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_tickets",
                "description": "List inbox tickets (open GitHub/Jira/Linear/Slack items the monitor has seen).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "state": { "type": "string", "description": "open | launched | closed" }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "finish_ticket",
                "description": "Load one inbox item (ENG-123, owner/repo#12, or github:owner/repo#12) and return its spec. Does not launch agents. You then integration_query and/or launch_agent_plan for whatever this item actually needs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ticket": { "type": "string" },
                        "provider": { "type": "string" },
                        "key": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            }
        }
    ])
}

pub fn system_prompt_with_tools() -> &'static str {
    "You are the Agent Fleet assistant. The user talks in normal language. You do the work.\n\n\
A sentence is enough. Compose tools from the request — there is no fixed playbook.\n\
- One-shot read/write on a connected app: integration_query(provider, operation, args).\n\
- Multi-step or sandbox work: launch_agent_plan with the tasks YOU choose (any worker_profile, depends_on, outputs).\n\
- Inbox item: finish_ticket to load the spec, then you plan (do not assume implement + review + PR).\n\
- Long-running wait/watch policy: start_case.\n\n\
Bias to action. Do not wait for confirmation. Do not invent slash-commands. Do not tell the user to open Settings first unless no model can run.\n\
The model in this composer is used for YOU and every sandbox worker. Do not call configure_fleet to switch providers.\n\
If a tool fails, retry with fixed args or another op. Do not stop after one error.\n\n\
Tools:\n\
- integration_query / list_integrations / get_integration — backend connector ops (full catalog is appended below)\n\
- launch_agent_plan / launch_agent_task / list_agent_personas\n\
- Plan outputs fire when a run ends ({{summary}} {{run_id}} {{status}}); workers also use_integration mid-run\n\
- get_fleet_status / list_sandboxes / list_tasks / list_runs / read_team_chat / send_team_message\n\
- monitor_tickets / list_tickets / finish_ticket (load spec only)\n\
- start_case / list_cases / get_case / observe_case / cancel_case\n\n\
Do NOT claim an external write succeeded unless integration_query (or a finished plan output) returned ok.\n\
If an app is disconnected, say Connect under Integrations and still do everything else.\n\
After launch: one sentence, who + run id. Never ask the user to say continue.\n\
Never create a Notion page without args.text (the actual result).\n\
Be concise."
}

pub fn last_user_text(history: &[Value]) -> String {
    history
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string()
}

/// Model tried to stop and ask the human to continue after launching a fleet.
pub fn rewrite_if_asked_to_continue(content: &str, actions: &[ChatAction]) -> Option<String> {
    let launched = actions
        .iter()
        .find(|a| a.ok && matches!(a.tool.as_str(), "launch_agent_plan" | "launch_agent_task"))?;
    let lower = content.to_ascii_lowercase();
    let asks = lower.contains("continue")
        || lower.contains("say \"continue\"")
        || lower.contains("blocked mid")
        || lower.contains("i'm blocked")
        || lower.contains("when it finishes")
        || lower.contains("when done")
        || lower.contains("when the run")
        || lower.contains("outputs will")
        || lower.contains("they'll implement")
        || lower.contains("they will implement");
    if !asks {
        return None;
    }
    let run = launched.run_id.as_deref().unwrap_or("the open run");
    Some(format!(
        "Fleet `{run}` is live on this screen. Stay here — task status and agent activity show in the live panel. \
I will not ask you to continue."
    ))
}

/// Full connector allowlist, plus which of those are connected in this workspace.
pub fn connector_catalog_markdown(connected: &[String]) -> String {
    let connected_set: std::collections::HashSet<&str> =
        connected.iter().map(|s| s.as_str()).collect();
    let mut out = String::from(
        "\n\n# Connector catalog (backend tools — call integration_query / use_integration)\n",
    );
    for provider in supported_providers() {
        let state = if connected_set.contains(*provider) {
            "connected"
        } else {
            "not connected"
        };
        let ops = operations_for(provider);
        out.push_str(&format!("- {provider} [{state}]: {}\n", ops.join(", ")));
    }
    if connected.is_empty() {
        out.push_str("None connected yet. User can connect them under Integrations.\n");
    }
    out
}

pub async fn system_prompt_for_workspace(state: &AppState, workspace_id: Uuid) -> String {
    let mut base = system_prompt_with_tools().to_string();
    let mut connected: Vec<String> = Vec::new();
    match build_integrations_snapshot(state, workspace_id, false).await {
        Ok((content, _)) => {
            if let Some(arr) = content.get("integrations").and_then(|v| v.as_array()) {
                connected = arr
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
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "integrations snapshot for system prompt failed");
        }
    }
    let ids: Vec<String> = connected
        .iter()
        .map(|s| s.split_whitespace().next().unwrap_or(s).to_string())
        .collect();
    base.push_str(&connector_catalog_markdown(&ids));
    if !connected.is_empty() {
        base.push_str(&format!("Connected accounts: {}.\n", connected.join(", ")));
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
struct OutputArg {
    provider: String,
    operation: String,
    #[serde(default)]
    args: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct LaunchPlanArgs {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    max_agents: Option<i32>,
    #[serde(default)]
    context_from: Option<Vec<ContextFromArg>>,
    #[serde(default)]
    outputs: Option<Vec<OutputArg>>,
    tasks: Vec<PlanTaskArg>,
}

#[derive(Debug, Deserialize)]
struct ListTasksArgs {
    #[serde(default)]
    status: Option<String>,
}

/// Provider + model selected in the chat composer. Workers use this exact pair.
#[derive(Debug, Clone, Default)]
pub struct ChatLlmContext {
    pub provider: String,
    pub model: String,
    pub runner_id: Option<Uuid>,
    pub effort: Option<String>,
}

/// Composer dropdown first (last chat in this workspace), then Grok/Ollama host defaults.
pub async fn composer_or_fallback(state: &AppState, workspace_id: Uuid) -> ChatLlmContext {
    if let Ok(conns) =
        fleet_db::repos::integrations::list_for_workspace(&state.db, workspace_id).await
    {
        for c in conns {
            let d = fleet_db::IntegrationAgentDefaults::from_config(&c.config);
            if let (Some(provider), Some(model)) = (d.default_provider, d.default_model) {
                return ChatLlmContext {
                    provider,
                    model,
                    runner_id: d
                        .default_runner_id
                        .as_deref()
                        .and_then(|s| Uuid::parse_str(s).ok()),
                    effort: None,
                };
            }
        }
    }
    if let Ok(Some((provider, model))) =
        fleet_db::repos::chats::latest_composer_llm(&state.db, workspace_id).await
    {
        return ChatLlmContext {
            provider,
            model,
            runner_id: None,
            effort: None,
        };
    }
    if state.grok.enabled {
        return ChatLlmContext {
            provider: "grok".into(),
            model: crate::grok::default_model(),
            runner_id: None,
            effort: None,
        };
    }
    if state.ollama.enabled {
        return ChatLlmContext {
            provider: "ollama".into(),
            model: state.ollama.default_model.clone().unwrap_or_default(),
            runner_id: None,
            effort: None,
        };
    }
    ChatLlmContext::default()
}

/// Chat tools are bound to the HTTP session workspace. A payload that names
/// another workspace is forbidden even if the caller is a member of both.
pub(crate) fn require_session_workspace(
    session_workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<()> {
    let obj = parse_json_args(arguments).unwrap_or_else(|_| arguments.clone());
    for requested in requested_workspace_ids(&obj) {
        if requested != session_workspace_id {
            return Err(AppError::forbidden(
                "workspace_id does not match this chat session",
            ));
        }
    }
    Ok(())
}

fn requested_workspace_ids(obj: &Value) -> Vec<Uuid> {
    let mut out = Vec::new();
    if let Some(id) = parse_workspace_id_field(obj.get("workspace_id")) {
        out.push(id);
    }
    if let Some(id) = obj
        .get("args")
        .and_then(|a| parse_workspace_id_field(a.get("workspace_id")))
    {
        out.push(id);
    }
    out
}

fn parse_workspace_id_field(v: Option<&Value>) -> Option<Uuid> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Uuid::parse_str(s.trim()).ok();
    }
    None
}

pub async fn execute_tool(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    name: &str,
    arguments: &Value,
    llm: &ChatLlmContext,
) -> (Value, ChatAction) {
    if let Err(e) = require_session_workspace(workspace_id, arguments) {
        let msg = e.to_string();
        return (
            json!({ "error": msg }),
            ChatAction {
                tool: name.to_string(),
                ok: false,
                summary: msg,
                run_id: None,
                sandbox_ids: None,
                detail: None,
            },
        );
    }
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
        "configure_fleet" => configure_fleet_tool(state, workspace_id, arguments).await,
        "launch_agent_task" => match parse_launch_args(arguments) {
            Ok(args) => launch_agent_task(state, workspace_id, user_id, args, llm).await,
            Err(e) => Err(AppError::bad_request(e)),
        },
        "launch_agent_plan" => match parse_launch_plan_args(arguments) {
            Ok(args) => launch_agent_plan(state, workspace_id, user_id, args, llm).await,
            Err(e) => Err(AppError::bad_request(e)),
        },
        "list_agent_personas" => {
            let div = arguments
                .get("division")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty());
            list_agent_personas_tool(state, workspace_id, div.as_deref()).await
        }
        "read_team_chat" => read_team_chat_tool(state, workspace_id, arguments).await,
        "send_team_message" => send_team_message_tool(state, workspace_id, arguments).await,
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
        "start_case" => start_case_tool(state, workspace_id, user_id, arguments).await,
        "list_cases" => {
            let filter = arguments
                .get("status")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            list_cases_tool(state, workspace_id, filter).await
        }
        "get_case" => get_case_tool(state, workspace_id, arguments).await,
        "observe_case" => observe_case_tool(state, workspace_id, arguments).await,
        "cancel_case" => cancel_case_tool(state, workspace_id, arguments).await,
        "monitor_tickets" => {
            let args = serde_json::from_value::<crate::tickets::MonitorArgs>(arguments.clone())
                .unwrap_or(crate::tickets::MonitorArgs {
                    enabled: None,
                    providers: None,
                    github_repos: None,
                    jira_jql: None,
                    slack_channels: None,
                    notion_databases: None,
                    auto_launch: None,
                });
            match crate::tickets::monitor_tickets(state, workspace_id, user_id, args).await {
                Ok(v) => Ok((
                    v.clone(),
                    ChatAction {
                        tool: "monitor_tickets".into(),
                        ok: true,
                        summary: "Inbox monitor updated".into(),
                        run_id: None,
                        sandbox_ids: None,
                        detail: Some(v),
                    },
                )),
                Err(e) => Err(e),
            }
        }
        "list_tickets" => {
            let filter = arguments
                .get("state")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match crate::tickets::list_tickets(state, workspace_id, filter).await {
                Ok(v) => {
                    let n = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
                    Ok((
                        v.clone(),
                        ChatAction {
                            tool: "list_tickets".into(),
                            ok: true,
                            summary: format!("{n} ticket(s)"),
                            run_id: None,
                            sandbox_ids: None,
                            detail: Some(v),
                        },
                    ))
                }
                Err(e) => Err(e),
            }
        }
        "finish_ticket" => {
            let args =
                serde_json::from_value::<crate::tickets::FinishTicketArgs>(arguments.clone())
                    .unwrap_or(crate::tickets::FinishTicketArgs {
                        ticket: arguments
                            .get("ticket")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        provider: arguments
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        key: arguments
                            .get("key")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    });
            match crate::tickets::ticket_context_for_model(state, workspace_id, user_id, args).await
            {
                Ok(v) => Ok((
                    v.clone(),
                    ChatAction {
                        tool: "finish_ticket".into(),
                        ok: true,
                        summary: v
                            .get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("ticket loaded")
                            .to_string(),
                        run_id: None,
                        sandbox_ids: None,
                        detail: Some(v),
                    },
                )),
                Err(e) => Err(e),
            }
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

/// Run a tool. Transient errors (network / 429 / 5xx) retry the **same**
/// call. Bad args may be filled from earlier results. Then the model still
/// sees a retry hint so it can redo the user task (any provider).
pub async fn execute_tool_with_retry(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    name: &str,
    arguments: &Value,
    llm: &ChatLlmContext,
    prior: &[Value],
) -> (Value, ChatAction) {
    let (mut content, mut action) =
        execute_tool(state, workspace_id, user_id, name, arguments, llm).await;

    // Fix args from earlier results *before* replaying a 400 with the same body.
    if !action.ok && name == "integration_query" {
        let obj = parse_json_args(arguments).unwrap_or_else(|_| arguments.clone());
        let provider = obj
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let operation = obj
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let args = obj.get("args").cloned().unwrap_or_else(|| json!({}));
        if let Some(fixed) = coerce_integration_args(&provider, &operation, &args, prior) {
            let retry_args = json!({
                "provider": provider,
                "operation": operation,
                "args": fixed,
            });
            tracing::info!(
                provider = %provider,
                operation = %operation,
                "integration_query retry with coerced args"
            );
            let (retry_content, retry_action) =
                execute_tool(state, workspace_id, user_id, name, &retry_args, llm).await;
            if retry_action.ok {
                let mut out = retry_content;
                if let Some(map) = out.as_object_mut() {
                    map.insert("retried".into(), json!(true));
                    map.insert("retry_from".into(), json!("coerced_args"));
                }
                let mut act = retry_action;
                act.summary = format!("retried: {}", act.summary);
                return (out, act);
            }
            content = retry_content;
            action = retry_action;
        }
    }

    let mut transient = 0u32;
    while !action.ok
        && crate::retry::is_retryable_tool(&action, &content)
        && transient < crate::retry::TOOL_TRANSIENT_RETRIES
    {
        transient += 1;
        let wait = std::time::Duration::from_millis(300 * u64::from(transient));
        tracing::warn!(
            tool = %name,
            attempt = transient,
            error = %action.summary,
            "retrying failed tool"
        );
        tokio::time::sleep(wait).await;
        let (c, a) = execute_tool(state, workspace_id, user_id, name, arguments, llm).await;
        content = c;
        action = a;
    }
    if action.ok {
        if transient > 0 {
            if let Some(map) = content.as_object_mut() {
                map.insert("retried".into(), json!(true));
                map.insert("retry_from".into(), json!("transient"));
            }
            action.summary = format!("retried: {}", action.summary);
        }
        return (content, action);
    }

    if name != "integration_query" {
        if let Some(map) = content.as_object_mut() {
            map.insert(
                "hint".into(),
                json!("Tool failed. Retry the same user task — call this tool again."),
            );
        }
        return (content, action);
    }

    let obj = parse_json_args(arguments).unwrap_or_else(|_| arguments.clone());
    let provider = obj
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let operation = obj
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if let Some(map) = content.as_object_mut() {
        map.insert("retry".into(), retry_hint(&provider, &operation));
        map.insert(
            "hint".into(),
            json!("Call integration_query again with retry.example. Do not stop."),
        );
    }
    action.summary = format!("{} — retry with the args in retry.example", action.summary);
    (content, action)
}

pub fn retry_hint(provider: &str, operation: &str) -> Value {
    json!({
        "should_retry": true,
        "provider": provider,
        "operation": operation,
        "required": required_args(provider, operation),
        "example": example_args(provider, operation),
    })
}

fn required_args(provider: &str, operation: &str) -> &'static [&'static str] {
    match (provider, operation) {
        ("notion", "create_page") => &["parent_page_id or parent_database_id", "title", "text"],
        ("notion", "append_blocks") => &["page_id", "text"],
        ("slack", "post_message") => &["channel", "text"],
        ("slack", "get_channel_history" | "get_thread") => &["channel"],
        ("github", "list_issues" | "get_issue" | "create_pr" | "add_comment") => &["owner", "repo"],
        _ => &[],
    }
}

fn example_args(provider: &str, operation: &str) -> Value {
    match (provider, operation) {
        ("notion", "create_page") => json!({
            "parent_page_id": "<id from notion search>",
            "title": "Slack #eng dump",
            "text": "<paste the Slack messages you already fetched>"
        }),
        ("notion", "append_blocks") => json!({
            "page_id": "<page id>",
            "text": "<the Slack messages>"
        }),
        ("slack", "post_message") => json!({ "channel": "C012", "text": "hello" }),
        ("slack", "get_channel_history") => json!({ "channel": "C012", "limit": 20 }),
        ("github", "list_issues") => json!({ "owner": "acme", "repo": "app" }),
        _ => json!({}),
    }
}

/// Fill missing args from earlier successful tool payloads in this turn.
pub fn coerce_integration_args(
    provider: &str,
    operation: &str,
    args: &Value,
    prior: &[Value],
) -> Option<Value> {
    let mut next = args.clone();
    let mut changed = false;

    if matches!(
        (provider, operation),
        ("notion", "create_page") | ("notion", "append_blocks")
    ) && write_text_missing(&next)
    {
        if let Some(text) = harvest_slack_text(prior) {
            next.as_object_mut()?.insert("text".into(), json!(text));
            changed = true;
        }
    }
    if matches!((provider, operation), ("notion", "create_page"))
        && next
            .get("parent_page_id")
            .and_then(|v| v.as_str())
            .is_none()
        && next
            .get("parent_database_id")
            .and_then(|v| v.as_str())
            .is_none()
    {
        if let Some(id) = harvest_notion_parent(prior) {
            next.as_object_mut()?
                .insert("parent_page_id".into(), json!(id));
            changed = true;
        }
    }
    if matches!(
        (provider, operation),
        ("slack", "post_message") | ("slack", "get_channel_history") | ("slack", "get_thread")
    ) {
        let ch = next.get("channel").and_then(|v| v.as_str()).unwrap_or("");
        let needs_id = ch.is_empty() || !(ch.starts_with('C') && ch.len() >= 8);
        if needs_id {
            if let Some(id) =
                harvest_slack_channel(prior).or_else(|| harvest_slack_channel_named(prior, ch))
            {
                next.as_object_mut()?.insert("channel".into(), json!(id));
                changed = true;
            }
        }
    }
    if changed {
        Some(next)
    } else {
        None
    }
}

fn write_text_missing(args: &Value) -> bool {
    for key in ["text", "content", "body", "message"] {
        if args
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
        {
            return false;
        }
    }
    true
}

fn harvest_slack_text(prior: &[Value]) -> Option<String> {
    let mut lines = Vec::new();
    for p in prior {
        if p.get("provider").and_then(|v| v.as_str()) != Some("slack") {
            continue;
        }
        if p.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        let items = p
            .pointer("/data/items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for item in items {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()).map(str::trim) {
                if !t.is_empty() {
                    let who = item.get("user").and_then(|v| v.as_str()).unwrap_or("slack");
                    lines.push(format!("{who}: {t}"));
                }
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn harvest_slack_channel_named(prior: &[Value], raw: &str) -> Option<String> {
    let want = raw.trim().trim_start_matches('#').to_ascii_lowercase();
    if want.is_empty() {
        return None;
    }
    for p in prior {
        if p.get("provider").and_then(|v| v.as_str()) != Some("slack") {
            continue;
        }
        let items = p
            .pointer("/data/items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for c in items {
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if !id.is_empty() && (name == want || id.eq_ignore_ascii_case(raw.trim())) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn harvest_slack_channel(prior: &[Value]) -> Option<String> {
    for p in prior {
        if p.get("provider").and_then(|v| v.as_str()) != Some("slack") {
            continue;
        }
        if let Some(ch) = p.pointer("/data/channel").and_then(|v| v.as_str()) {
            if !ch.is_empty() {
                return Some(ch.to_string());
            }
        }
        if let Some(id) = p
            .pointer("/data/items/0/id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn harvest_notion_parent(prior: &[Value]) -> Option<String> {
    for p in prior {
        if p.get("provider").and_then(|v| v.as_str()) != Some("notion") {
            continue;
        }
        if let Some(id) = p
            .pointer("/data/id")
            .or_else(|| p.pointer("/data/items/0/id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(id.to_string());
        }
    }
    None
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
            acceptance: fleet_orchestrator::orch::default_acceptance(worker_profile),
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        }],
    )
}

fn stamp_plan_llm(plan: &mut WorkPlanBody, llm: &ChatLlmContext) {
    let provider = llm.provider.trim();
    let model = llm.model.trim();
    if !provider.is_empty() {
        plan.policy.llm_provider = Some(provider.to_ascii_lowercase());
    }
    if !model.is_empty() {
        plan.policy.llm_model = Some(model.to_string());
    }
    if let Some(id) = llm.runner_id {
        plan.policy.preferred_host_id = Some(id);
    }
    if let Some(e) = llm
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        plan.policy.reasoning_effort = Some(e.to_string());
    }
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
            ..Default::default()
        },
    }
}

async fn ensure_project(state: &AppState, workspace_id: Uuid) -> AppResult<()> {
    state
        .orch
        .store
        .ensure_project(workspace_id, "workspace fleet", 3)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, error = %e, "chat tool ensure_project failed");
            AppError::internal(e.to_string())
        })?;
    Ok(())
}

async fn ensure_project_capacity(
    state: &AppState,
    workspace_id: Uuid,
    max_agents: i32,
) -> AppResult<()> {
    let max = max_agents.clamp(1, chat_max_agents());
    ensure_project(state, workspace_id).await?;
    state
        .orch
        .store
        .set_max_agents(workspace_id, max)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, error = %e, "set_max_agents failed");
            AppError::internal(e.to_string())
        })?;
    Ok(())
}

async fn prefetch_context(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    context_from: &[ContextFromArg],
) -> AppResult<Vec<ProxyResult>> {
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
        if !result.ok {
            let code = result.error_code.as_deref().unwrap_or("error");
            return Err(AppError::bad_request(format!(
                "context_from {} `{}` is {code}: {}. Connect the provider under Workspace → Integrations.",
                result.provider, result.operation, result.summary
            )));
        }
        out.push(result);
    }
    Ok(out)
}

async fn integration_query(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    require_session_workspace(workspace_id, arguments)?;
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
            .map_err(|e| AppError::internal(e.to_string()))?;
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

async fn list_agent_personas_tool(
    state: &AppState,
    workspace_id: Uuid,
    division: Option<&str>,
) -> AppResult<(Value, ChatAction)> {
    let data_dir = std::env::var("DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("./data"));
    let mut personas = fleet_orchestrator::personas::list_personas(&data_dir).await;
    if let Some(d) = division {
        personas.retain(|p| p.division.eq_ignore_ascii_case(d));
    }
    let custom =
        fleet_db::repos::personas::list_for_workspace(&state.db, workspace_id, Some("active"))
            .await
            .unwrap_or_default();
    let custom_json: Vec<Value> = custom
        .iter()
        .map(|p| {
            json!({
                "slug": p.slug,
                "name": p.name,
                "description": p.description,
                "division": "workspace",
                "source": "workspace",
            })
        })
        .collect();
    let n = personas.len() + custom_json.len();
    let content = json!({
        "source": "https://github.com/msitarzewski/agency-agents",
        "count": n,
        "default": "generalist",
        "workspace_personas": custom_json,
        "aliases": {
            "generalist": "generalist",
            "coding": "engineering-backend-architect (optional shortcut)",
            "frontend": "engineering-frontend-developer (optional shortcut)",
            "reviewer": "any *reviewer* slug (optional shortcut)",
            "testing": "testing/* first match (optional shortcut)",
        },
        "personas": personas.iter().take(80).collect::<Vec<_>>(),
        "hint": "Set worker_profile to a workspace persona slug (Agents page) or any roster slug.",
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_agent_personas".into(),
            ok: true,
            summary: format!("{n} persona(s)"),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

fn orch_data_dir(state: &AppState) -> std::path::PathBuf {
    state.orch.tick.data_dir().to_path_buf()
}

async fn resolve_chat_run(
    state: &AppState,
    workspace_id: Uuid,
    run_id: Option<&str>,
) -> AppResult<Uuid> {
    ensure_project(state, workspace_id).await?;
    if let Some(raw) = run_id.map(str::trim).filter(|s| !s.is_empty()) {
        let id =
            Uuid::parse_str(raw).map_err(|_| AppError::bad_request("run_id must be a UUID"))?;
        let run = state
            .orch
            .store
            .get_run(workspace_id, id)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .ok_or_else(|| AppError::not_found("run not found"))?;
        return Ok(run.id);
    }
    let runs = state
        .orch
        .store
        .list_runs(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let open = runs
        .iter()
        .find(|r| !matches!(r.status.as_str(), "completed" | "failed" | "cancelled"));
    open.or(runs.first())
        .map(|r| r.id)
        .ok_or_else(|| AppError::bad_request("no fleet runs yet"))
}

async fn start_case_tool(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    require_session_workspace(workspace_id, arguments)?;
    let obj = parse_json_args(arguments).map_err(AppError::bad_request)?;
    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("title is required"))?
        .to_string();
    let policy_md = obj
        .get("policy_md")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let stage = obj
        .get("stage")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let wake_in_s = obj.get("wake_in_s").and_then(|v| v.as_i64());
    let context = obj.get("context").cloned();
    let case = crate::cases::create_case(
        state,
        workspace_id,
        Some(user_id),
        crate::cases::CreateCaseBody {
            title: title.clone(),
            kind,
            policy_md: Some(policy_md),
            stage,
            context,
            wake_in_s,
        },
    )
    .await?;
    let content = crate::cases::case_json(&case, None);
    Ok((
        content.clone(),
        ChatAction {
            tool: "start_case".into(),
            ok: true,
            summary: format!(
                "Case {} started (stage={}, status={}) — backend will wake and decide",
                case.id, case.stage, case.status
            ),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn list_cases_tool(
    state: &AppState,
    workspace_id: Uuid,
    status_filter: Option<&str>,
) -> AppResult<(Value, ChatAction)> {
    let rows =
        fleet_db::repos::cases::list_for_workspace(&state.db, workspace_id, status_filter, 50)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    let content = json!({
        "count": rows.len(),
        "cases": rows.iter().map(|c| crate::cases::case_json(c, None)).collect::<Vec<_>>(),
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "list_cases".into(),
            ok: true,
            summary: format!("{} case(s)", rows.len()),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn get_case_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let id = arguments
        .get("case_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("case_id is required"))?;
    let case_id =
        Uuid::parse_str(id.trim()).map_err(|_| AppError::bad_request("case_id must be a UUID"))?;
    let case = fleet_db::repos::cases::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    let events = fleet_db::repos::cases::list_events(&state.db, case_id, 40)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let content = crate::cases::case_json(&case, Some(&events));
    Ok((
        content.clone(),
        ChatAction {
            tool: "get_case".into(),
            ok: true,
            summary: format!("Case {} — {} / {}", case.id, case.status, case.stage),
            run_id: case.run_id.map(|r| r.to_string()),
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn observe_case_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    require_session_workspace(workspace_id, arguments)?;
    let id = arguments
        .get("case_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("case_id is required"))?;
    let case_id =
        Uuid::parse_str(id.trim()).map_err(|_| AppError::bad_request("case_id must be a UUID"))?;
    let note = arguments
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let wake = arguments
        .get("wake")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let case = crate::cases::observe_case(state, workspace_id, case_id, &note, wake).await?;
    let content = crate::cases::case_json(&case, None);
    Ok((
        content.clone(),
        ChatAction {
            tool: "observe_case".into(),
            ok: true,
            summary: format!("Observed case {} (wake={wake})", case.id),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn cancel_case_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let id = arguments
        .get("case_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("case_id is required"))?;
    let case_id =
        Uuid::parse_str(id.trim()).map_err(|_| AppError::bad_request("case_id must be a UUID"))?;
    let case = crate::cases::cancel_case(state, workspace_id, case_id).await?;
    let content = crate::cases::case_json(&case, None);
    Ok((
        content.clone(),
        ChatAction {
            tool: "cancel_case".into(),
            ok: true,
            summary: format!("Case {} cancelled", case.id),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn read_team_chat_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let run_id = resolve_chat_run(
        state,
        workspace_id,
        arguments.get("run_id").and_then(|v| v.as_str()),
    )
    .await?;
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(40)
        .clamp(1, 200) as usize;
    let talk = fleet_orchestrator::sandbox::team_chat::TeamTalk::from_spawn(
        &orch_data_dir(state),
        workspace_id,
        run_id,
        "human",
        "human",
    );
    let mut msgs = fleet_orchestrator::sandbox::team_chat::list_messages(&talk).await;
    if msgs.len() > limit {
        msgs = msgs.split_off(msgs.len() - limit);
    }
    let items: Vec<Value> = msgs
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "seq": m.seq,
                "ts": m.ts,
                "from": m.from,
                "from_profile": m.from_profile,
                "to": m.to,
                "kind": m.kind,
                "in_reply_to": m.in_reply_to,
                "body": m.body,
            })
        })
        .collect();
    let n = items.len();
    let content = json!({
        "run_id": run_id,
        "count": n,
        "messages": items,
        "transcript": fleet_orchestrator::sandbox::team_chat::render_team_chat_md(&msgs),
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "read_team_chat".into(),
            ok: true,
            summary: format!("{n} team message(s) on run {run_id}"),
            run_id: Some(run_id.to_string()),
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn send_team_message_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    let body = arguments
        .get("body")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("body is required"))?;
    let to = arguments.get("to").and_then(|v| v.as_str()).unwrap_or("*");
    let run_id = resolve_chat_run(
        state,
        workspace_id,
        arguments.get("run_id").and_then(|v| v.as_str()),
    )
    .await?;
    let talk = fleet_orchestrator::sandbox::team_chat::TeamTalk::from_spawn(
        &orch_data_dir(state),
        workspace_id,
        run_id,
        "human",
        "human",
    );
    let msg = fleet_orchestrator::sandbox::team_chat::post_message(&talk, to, "say", body, None)
        .await
        .map_err(AppError::bad_request)?;
    let content = json!({
        "run_id": run_id,
        "id": msg.id,
        "seq": msg.seq,
        "from": msg.from,
        "to": msg.to,
        "kind": msg.kind,
        "body": msg.body,
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "send_team_message".into(),
            ok: true,
            summary: format!("sent to `{}` on run {run_id}", msg.to),
            run_id: Some(run_id.to_string()),
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn list_sandboxes(state: &AppState, workspace_id: Uuid) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id).await?;
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
    ensure_project(state, workspace_id).await?;
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
    ensure_project(state, workspace_id).await?;
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

async fn configure_fleet_tool(
    state: &AppState,
    workspace_id: Uuid,
    arguments: &Value,
) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id).await?;
    let current = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    let max = arguments
        .get("max_agents")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .unwrap_or(current.max_agents);
    if max < 1 || max > fleet_orchestrator::domain::MAX_PROJECT_AGENTS {
        return Err(AppError::bad_request(format!(
            "max_agents must be 1–{}",
            fleet_orchestrator::domain::MAX_PROJECT_AGENTS
        )));
    }

    let llm = arguments
        .get("fleet_llm")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(current.fleet_llm.as_str());
    if !matches!(llm, "off" | "ollama") {
        return Err(AppError::bad_request(
            "fleet_llm must be off or ollama (Grok is env-only)",
        ));
    }
    let model = arguments
        .get("fleet_llm_model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| current.fleet_llm_model.clone());

    let cfg = state
        .orch
        .store
        .update_fleet_settings(workspace_id, max, llm, &model)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let llm_res = fleet_orchestrator::sandbox::agent_loop::resolve_workspace_llm(
        &cfg.fleet_llm,
        &cfg.fleet_llm_model,
    );
    let content = json!({
        "max_agents": cfg.max_agents,
        "fleet_llm": cfg.fleet_llm,
        "fleet_llm_model": cfg.fleet_llm_model,
        "effective_backend": llm_res.backend,
        "effective_model": llm_res.model,
        "grok_env": fleet_orchestrator::sandbox::agent_loop::grok_env_enabled(),
    });
    Ok((
        content.clone(),
        ChatAction {
            tool: "configure_fleet".into(),
            ok: true,
            summary: format!(
                "fleet llm={} model={}",
                llm_res.backend.as_deref().unwrap_or("off"),
                llm_res.model.as_deref().unwrap_or("-")
            ),
            run_id: None,
            sandbox_ids: None,
            detail: Some(content),
        },
    ))
}

async fn get_fleet_status(state: &AppState, workspace_id: Uuid) -> AppResult<(Value, ChatAction)> {
    ensure_project(state, workspace_id).await?;
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
        .filter(|r| !matches!(r.status.as_str(), "completed" | "failed" | "cancelled"))
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
        "fleet_llm": cfg.fleet_llm,
        "fleet_llm_model": cfg.fleet_llm_model,
        "effective_backend": fleet_orchestrator::sandbox::agent_loop::resolve_workspace_llm(
            &cfg.fleet_llm,
            &cfg.fleet_llm_model,
        )
        .backend,
        "grok_env": fleet_orchestrator::sandbox::agent_loop::grok_env_enabled(),
        "hint": "Workers use this chat's provider+model. Just launch_agent_plan. Integrations are optional.",
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
                "{open_runs} open run(s), {active_sandboxes} active sandbox(es), llm={}",
                content
                    .get("effective_backend")
                    .and_then(|v| v.as_str())
                    .unwrap_or("off")
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
    llm: &ChatLlmContext,
) -> AppResult<(Value, ChatAction)> {
    let max_agents = args
        .max_agents
        .unwrap_or_else(chat_max_agents)
        .clamp(1, chat_max_agents());
    ensure_project_capacity(state, workspace_id, max_agents).await?;

    let profile = args
        .worker_profile
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("generalist");
    let mut spec = args
        .spec
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    if spec.len() > 32_000 {
        return Err(AppError::bad_request("spec too long (max 32k bytes)"));
    }

    let ctx = if let Some(ref cf) = args.context_from {
        prefetch_context(state, workspace_id, user_id, cf).await?
    } else {
        Vec::new()
    };
    if !ctx.is_empty() {
        spec.push_str(&format_context_markdown(&ctx));
    }

    let mut plan = build_chat_plan(workspace_id, &args.title, &spec, profile);
    stamp_plan_llm(&mut plan, llm);
    accept_and_drive_plan(
        state,
        workspace_id,
        plan,
        "launch_agent_task",
        &args.title,
        ctx,
    )
    .await
}

async fn launch_agent_plan(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: LaunchPlanArgs,
    llm: &ChatLlmContext,
) -> AppResult<(Value, ChatAction)> {
    let max_agents = args
        .max_agents
        .unwrap_or_else(chat_max_agents)
        .clamp(1, chat_max_agents());
    ensure_project_capacity(state, workspace_id, max_agents).await?;

    let ctx = if let Some(ref cf) = args.context_from {
        prefetch_context(state, workspace_id, user_id, cf).await?
    } else {
        Vec::new()
    };
    let ctx_md = if ctx.is_empty() {
        String::new()
    } else {
        format_context_markdown(&ctx)
    };

    let mut tasks = Vec::with_capacity(args.tasks.len());
    for t in &args.tasks {
        let mut spec = t
            .spec
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();
        if spec.len() > 32_000 {
            return Err(AppError::bad_request("spec too long (max 32k bytes)"));
        }
        if !ctx_md.is_empty() {
            spec.push_str(&ctx_md);
        }
        if spec.len() > 32_000 {
            return Err(AppError::bad_request("spec too long (max 32k bytes)"));
        }
        tasks.push(PlanTask {
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
                .unwrap_or("generalist")
                .to_string(),
            acceptance: fleet_orchestrator::orch::default_acceptance(
                t.worker_profile.as_deref().unwrap_or("generalist"),
            ),
            priority: t.priority.unwrap_or(100),
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        });
    }

    let mut plan = build_multi_task_plan(workspace_id, args.max_agents, tasks);
    if let Some(outs) = args.outputs {
        plan.policy.outputs = outs
            .into_iter()
            .filter(|o| !o.provider.trim().is_empty() && !o.operation.trim().is_empty())
            .map(|o| fleet_orchestrator::domain::PlanIo {
                provider: o.provider.trim().to_ascii_lowercase(),
                operation: o.operation.trim().to_ascii_lowercase(),
                args: o.args.unwrap_or(json!({})),
            })
            .collect();
    }
    stamp_plan_llm(&mut plan, llm);
    if let Err(e) = validate::validate_plan(&plan) {
        return Err(AppError::bad_request(e.to_string()));
    }

    let label = args
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("multi-agent plan");
    accept_and_drive_plan(state, workspace_id, plan, "launch_agent_plan", label, ctx).await
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
        .map_err(|e| AppError::internal(e.to_string()))?;
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
        "Fleet is running. Do NOT ask the user to continue. Plan outputs fire when the run completes. Watch Fleet."
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

    let summary = if terminal {
        format!(
            "Fleet finished «{label}» · {task_count} task(s) · run {} · terminal",
            short_id(&run_id.to_string()),
        )
    } else {
        format!(
            "Fleet running «{label}» · {task_count} agents/tasks · run {} · implement→review→ship in background — do not ask the user to continue",
            short_id(&run_id.to_string()),
        )
    };

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
        "gitlab" => {
            "list_repos, list_issues, get_issue, list_mrs, get_mr, get_file, list_commits, commit_files, create_branch, create_mr, create_issue, add_comment"
        }
        "jira" => {
            "search_issues, get_issue, list_projects, create_issue, add_comment, list_transitions, transition_issue"
        }
        "linear" => "list_issues, get_issue, list_teams, create_issue, update_issue, add_comment",
        "slack" => "list_channels, list_users, post_message, get_channel_history, get_thread",
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
        "trello" => {
            "list_boards, list_lists, list_cards, get_card, create_card, update_card, add_comment"
        }
        "asana" => {
            "list_workspaces, list_projects, list_tasks, get_task, create_task, update_task, add_comment"
        }
        "bitbucket" => {
            "list_repos, list_issues, get_issue, list_prs, get_pr, get_file, list_commits, commit_files, create_branch, create_pr, create_issue, add_comment"
        }
        "confluence" => {
            "search, list_spaces, list_pages, get_page, create_page, update_page, add_comment"
        }
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
            "operations": operations_for(&view.id),
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
        "operations": operations_for(provider),
        "connect_hint": if connected {
            "Already connected for this workspace."
        } else {
            "Not connected — user can Connect on the Integrations page (provider login + consent)."
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
                if connected {
                    "connected"
                } else {
                    "not connected"
                }
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
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    fn assert_forbidden(err: AppError) {
        let status = err.into_response().status();
        assert_eq!(status, StatusCode::FORBIDDEN, "expected 403");
    }

    #[test]
    fn start_case_foreign_workspace_id_is_forbidden() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let err = require_session_workspace(
            session,
            &json!({
                "title": "watch inbox",
                "policy_md": "wake on reply",
                "workspace_id": foreign.to_string(),
            }),
        )
        .expect_err("foreign start_case workspace_id");
        assert_forbidden(err);
    }

    #[test]
    fn observe_case_foreign_workspace_id_is_forbidden() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let err = require_session_workspace(
            session,
            &json!({
                "case_id": Uuid::new_v4().to_string(),
                "note": "they replied",
                "workspace_id": foreign.to_string(),
            }),
        )
        .expect_err("foreign observe_case workspace_id");
        assert_forbidden(err);
    }

    #[test]
    fn integration_query_foreign_workspace_id_is_forbidden() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let err = require_session_workspace(
            session,
            &json!({
                "provider": "slack",
                "operation": "list_channels",
                "workspace_id": foreign.to_string(),
            }),
        )
        .expect_err("foreign integration_query workspace_id");
        assert_forbidden(err);
    }

    #[test]
    fn integration_query_nested_args_foreign_workspace_id_is_forbidden() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let err = require_session_workspace(
            session,
            &json!({
                "provider": "github",
                "operation": "list_issues",
                "args": {
                    "owner": "acme",
                    "repo": "app",
                    "workspace_id": foreign.to_string(),
                },
            }),
        )
        .expect_err("foreign integration_query args.workspace_id");
        assert_forbidden(err);
    }

    #[test]
    fn chat_tool_session_membership_does_not_unlock_foreign_workspace() {
        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        for args in [
            json!({ "workspace_id": foreign, "title": "x", "policy_md": "y" }),
            json!({ "workspace_id": foreign.to_string(), "case_id": Uuid::new_v4() }),
            json!({
                "provider": "notion",
                "operation": "search",
                "workspace_id": foreign.to_string(),
            }),
        ] {
            let err = require_session_workspace(session, &args)
                .expect_err("membership on the session workspace is not enough");
            assert_forbidden(err);
        }
    }

    #[test]
    fn chat_tool_same_or_omitted_workspace_id_is_ok() {
        let session = Uuid::new_v4();
        require_session_workspace(session, &json!({ "title": "x", "policy_md": "y" }))
            .expect("omitted workspace_id uses the session");
        require_session_workspace(
            session,
            &json!({
                "title": "x",
                "policy_md": "y",
                "workspace_id": session.to_string(),
            }),
        )
        .expect("matching workspace_id");
        require_session_workspace(
            session,
            &json!({
                "provider": "slack",
                "operation": "list_channels",
                "args": { "workspace_id": session.to_string() },
            }),
        )
        .expect("matching nested workspace_id");
    }

    /// `send_team_message` / `read_team_chat` must not accept another
    /// workspace's `run_id` or a random UUID. Distinct from
    /// `require_session_workspace` (foreign `workspace_id` on other tools).
    #[test]
    fn resolve_chat_run_scopes_team_chat_to_workspace_run() {
        let not_found = AppError::not_found("run not found").into_response();
        assert_eq!(
            not_found.status(),
            StatusCode::NOT_FOUND,
            "unknown / foreign run_id is 404"
        );
        let bad = AppError::bad_request("run_id must be a UUID").into_response();
        assert_eq!(
            bad.status(),
            StatusCode::BAD_REQUEST,
            "malformed run_id is 400"
        );

        let src = production_src();
        let resolve = fn_src(src, "async fn resolve_chat_run");
        assert!(
            resolve.contains("get_run"),
            "resolve_chat_run must load the run in this workspace, not parse any UUID:\n{resolve}"
        );
        assert!(
            resolve.contains("get_run(workspace_id, id)")
                || resolve.contains("get_run(workspace_id,"),
            "get_run must be keyed by this workspace (project_id):\n{resolve}"
        );
        assert!(
            resolve.contains("AppError::not_found") || resolve.contains("not_found("),
            "missing / foreign run must fail closed as 404:\n{resolve}"
        );
        assert!(
            resolve.contains("AppError::bad_request") && resolve.contains("run_id must be a UUID"),
            "malformed run_id must stay 400:\n{resolve}"
        );
        let parse_at = resolve
            .find("Uuid::parse_str")
            .expect("explicit run_id must parse as UUID");
        let get_at = resolve
            .find("get_run")
            .expect("explicit run_id must call get_run");
        assert!(
            parse_at < get_at,
            "parse then get_run — do not return a parsed UUID that is not this workspace's run:\n{resolve}"
        );
        let after_parse = &resolve[parse_at..];
        assert!(
            !after_parse
                .lines()
                .take(4)
                .any(|l| l.contains("return Uuid::parse_str")
                    || (l.contains("return") && l.contains("parse_str") && !l.contains("get_run"))),
            "must not return the parsed UUID without get_run:\n{resolve}"
        );
        assert!(
            resolve.contains("list_runs(workspace_id)"),
            "omitted run_id must still resolve via list_runs of this workspace only:\n{resolve}"
        );
        assert!(
            !resolve.contains("list_runs()") && !resolve.contains("list_open_project_ids"),
            "omitted run_id must not scan other workspaces:\n{resolve}"
        );

        let send = fn_src(src, "async fn send_team_message_tool");
        let send_resolve = send
            .find("resolve_chat_run")
            .expect("send_team_message must resolve the run");
        let send_post = send
            .find("post_message")
            .expect("send_team_message must post only after resolve");
        assert!(
            send_resolve < send_post,
            "send_team_message must not write chat.jsonl before resolve_chat_run:\n{send}"
        );

        let read = fn_src(src, "async fn read_team_chat_tool");
        let read_resolve = read
            .find("resolve_chat_run")
            .expect("read_team_chat must resolve the run");
        let read_list = read
            .find("list_messages")
            .expect("read_team_chat must list only after resolve");
        assert!(
            read_resolve < read_list,
            "read_team_chat must not open a chat log before resolve_chat_run:\n{read}"
        );
    }

    #[test]
    fn stamp_plan_llm_writes_provider_and_model() {
        let mut plan = build_chat_plan(Uuid::nil(), "T", "S", "coding");
        stamp_plan_llm(
            &mut plan,
            &ChatLlmContext {
                provider: "Grok".into(),
                model: "grok-4.5".into(),
                runner_id: None,
                effort: None,
            },
        );
        assert_eq!(plan.policy.llm_provider.as_deref(), Some("grok"));
        assert_eq!(plan.policy.llm_model.as_deref(), Some("grok-4.5"));
    }

    #[test]
    fn parse_launch_plan_rejects_empty_and_blank_ids() {
        assert!(parse_launch_plan_args(&json!({ "tasks": [] })).is_err());
        assert!(parse_launch_plan_args(&json!({
            "tasks": [{ "id": " ", "title": "X" }]
        }))
        .is_err());
        let ok = parse_launch_plan_args(&json!({
            "outputs": [{
                "provider": "notion",
                "operation": "append_blocks",
                "args": { "page_id": "p1", "text": "{{summary}}" }
            }],
            "tasks": [
                { "id": "a", "title": "API", "worker_profile": "frontend" },
                { "id": "b", "title": "Review", "depends_on": ["a"] }
            ]
        }))
        .unwrap();
        assert_eq!(ok.tasks.len(), 2);
        assert_eq!(ok.tasks[0].worker_profile.as_deref(), Some("frontend"));
        let outs = ok.outputs.unwrap();
        assert_eq!(outs[0].provider, "notion");
        assert_eq!(outs[0].operation, "append_blocks");
    }

    #[test]
    fn build_chat_plan_sets_chat_source_and_single_task() {
        let ws = Uuid::new_v4();
        let plan = build_chat_plan(ws, "Fix flaky tests", "Investigate CI flake", "generalist");
        assert_eq!(plan.project_id, ws);
        assert_eq!(plan.source, "chat");
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].title, "Fix flaky tests");
        assert_eq!(plan.tasks[0].worker_profile, "generalist");
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

    /// Docs §7.1: `launch_agent_task` / `launch_agent_plan` titles must be
    /// non-empty and ≤ 200 chars. Empty, whitespace-only, and 201-char
    /// titles fail closed in parse (no plan/run insert). Exactly 200 still
    /// parses so this is not “every launch fails”. Distinct from the
    /// 1000-task cap. Live execute_tool + store lock is
    /// `launch_title_execute_tool_rejects_empty_and_over_200_and_inserts_no_run`.
    #[test]
    fn launch_title_must_be_non_empty_and_at_most_200() {
        let at_cap = "T".repeat(200);
        let over = "T".repeat(201);
        assert_eq!(at_cap.chars().count(), 200);
        assert_eq!(over.chars().count(), 201);
        assert_eq!(at_cap.len(), 200);
        assert_eq!(over.len(), 201);

        fn title_err_is_empty_or_too_long(err: &str, raw: &str) {
            let lower = err.to_ascii_lowercase();
            if raw.trim().is_empty() {
                assert!(
                    lower.contains("empty") && lower.contains("title"),
                    "blank title must fail as empty, not the 1000-task cap: {err}"
                );
                return;
            }
            assert!(
                lower.contains("title")
                    && (lower.contains("too long")
                        || lower.contains("max")
                        || lower.contains("200")),
                "201-char title must fail the 200-char title cap (not the task cap): {err}"
            );
            assert!(
                !lower.contains("too many") && !lower.contains("1000"),
                "title length is not the 1000-task cap: {err}"
            );
        }

        for bad in ["", "   ", over.as_str()] {
            let err = parse_launch_args(&json!({ "title": bad, "spec": "do it" })).expect_err(
                "launch_agent_task empty / whitespace / 201-char title must fail closed",
            );
            title_err_is_empty_or_too_long(&err, bad);
        }

        let task_cap = parse_launch_args(&json!({
            "title": at_cap,
            "spec": "do it",
        }))
        .expect("exactly 200-char launch_agent_task title must still parse");
        assert_eq!(task_cap.title, at_cap);

        let task_short = parse_launch_args(&json!({
            "title": "  Fix flaky tests  ",
            "spec": "details",
        }))
        .expect("normal short launch_agent_task title must still parse");
        assert_eq!(task_short.title, "Fix flaky tests");

        for bad in ["", "   ", over.as_str()] {
            let err = parse_launch_plan_args(&json!({
                "tasks": [{ "id": "t1", "title": bad, "spec": "do it" }]
            }))
            .expect_err(
                "launch_agent_plan empty / whitespace / 201-char task title must fail closed",
            );
            title_err_is_empty_or_too_long(&err, bad);
        }

        let plan_cap = parse_launch_plan_args(&json!({
            "tasks": [{ "id": "t1", "title": at_cap, "spec": "do it" }]
        }))
        .expect("exactly 200-char launch_agent_plan task title must still parse");
        assert_eq!(plan_cap.tasks.len(), 1);
        assert_eq!(plan_cap.tasks[0].title, at_cap);

        let plan_short = parse_launch_plan_args(&json!({
            "tasks": [{ "id": "t1", "title": "  Review PR  " }]
        }))
        .expect("normal short launch_agent_plan task title must still parse");
        assert_eq!(plan_short.tasks[0].title.trim(), "Review PR");

        let built = build_chat_plan(Uuid::new_v4(), &at_cap, "do it", "generalist");
        assert_eq!(built.tasks.len(), 1);
        assert_eq!(built.tasks[0].title, at_cap);
        validate::validate_plan(&built)
            .expect("200-char title must still be a valid 1-task chat plan");
    }

    #[test]
    fn chat_plan_cap_matches_orchestrator() {
        assert_eq!(MAX_CHAT_PLAN_TASKS, MAX_TASKS_PER_PLAN);
        assert!(MAX_CHAT_PLAN_TASKS >= 1000);
    }

    /// Docs §7.1: `launch_agent_task` ticks up to `MAX_CHAT_TICKS` (24)
    /// in-request so a stub/local worker can finish. A large
    /// `launch_agent_plan` DAG only does `MAX_CHAT_TICKS_LARGE` (3)
    /// in-request ticks; the process background ticker drains the rest.
    /// Must not drain a many-task plan inside the chat request.
    #[test]
    fn chat_ticks_large_plans_three_single_task_uses_twenty_four() {
        assert_eq!(
            MAX_CHAT_TICKS, 24,
            "docs §7.1: single-task launch ticks up to 24 in-request"
        );
        assert_eq!(
            MAX_CHAT_TICKS_LARGE, 3,
            "docs §7.1: large plans only do 3 in-request ticks"
        );
        assert!(
            MAX_CHAT_TICKS_LARGE < MAX_CHAT_TICKS,
            "large-plan budget must be strictly smaller than the single-task budget"
        );

        let single = build_chat_plan(Uuid::new_v4(), "One shot", "do it", "generalist");
        assert_eq!(
            single.tasks.len(),
            1,
            "launch_agent_task builds a 1-task plan, which must take the 24-tick budget"
        );

        let src = production_src();
        let task = fn_src(src, "async fn launch_agent_task");
        let plan = fn_src(src, "async fn launch_agent_plan");
        let accept = fn_src(src, "async fn accept_and_drive_plan");
        let tick = fn_src(src, "async fn tick_until_run_settled");
        let exec = fn_src(src, "pub async fn execute_tool");

        assert!(
            task.contains("build_chat_plan") && task.contains("accept_and_drive_plan"),
            "launch_agent_task must build the 1-task plan and drive it via accept_and_drive_plan:\n{task}"
        );
        assert!(
            plan.contains("accept_and_drive_plan"),
            "launch_agent_plan must drive via accept_and_drive_plan (same tick budget):\n{plan}"
        );
        for (label, body) in [
            ("launch_agent_task", task),
            ("launch_agent_plan", plan),
            ("execute_tool", exec),
        ] {
            assert!(
                !body.contains("tick_until_run_settled") && !body.contains("tick_project"),
                "{label} must not tick itself — only accept_and_drive_plan may in-request tick:\n{body}"
            );
            assert!(
                !body.contains("MAX_CHAT_TICKS"),
                "{label} must not pick its own tick budget (would skip the large-plan cap):\n{body}"
            );
        }

        assert!(
            accept.contains("plan.tasks.len()") && accept.contains("task_count"),
            "accept_and_drive_plan must size the in-request budget from the plan's task count:\n{accept}"
        );
        assert_eq!(
            accept.matches("tick_until_run_settled").count(),
            1,
            "exactly one in-request drain — a second loop would finish a large plan in the request:\n{accept}"
        );
        let tick_at = accept
            .find("tick_until_run_settled")
            .expect("accept_and_drive_plan must tick via tick_until_run_settled");
        let call = accept[tick_at..]
            .lines()
            .next()
            .expect("tick_until_run_settled call");
        assert!(
            call.contains("max_ticks"),
            "tick_until_run_settled must receive the computed budget, not a constant:\n{call}"
        );
        assert!(
            !call.contains("MAX_CHAT_TICKS"),
            "must not pass MAX_CHAT_TICKS / MAX_CHAT_TICKS_LARGE directly (skips the branch):\n{call}"
        );
        assert!(
            !accept[tick_at + "tick_until_run_settled".len()..].contains("tick_project")
                && !accept[tick_at + "tick_until_run_settled".len()..]
                    .contains("tick_until_run_settled"),
            "no extra in-request ticks after the capped drain:\n{accept}"
        );

        assert_eq!(
            in_request_ticks_for_task_count(accept, 1),
            MAX_CHAT_TICKS,
            "single-task launch must use MAX_CHAT_TICKS (24)"
        );
        for n in [9usize, 16, 32, 100, 1000, MAX_CHAT_PLAN_TASKS] {
            assert_eq!(
                in_request_ticks_for_task_count(accept, n),
                MAX_CHAT_TICKS_LARGE,
                "{n}-task plan is large: only MAX_CHAT_TICKS_LARGE (3) in-request ticks, \
                 not a {MAX_CHAT_TICKS}-tick drain:\n{accept}"
            );
        }

        assert!(
            tick.contains("for i in 0..max_ticks") || tick.contains("for _ in 0..max_ticks"),
            "tick_until_run_settled must be a finite 0..max_ticks loop (exclusive):\n{tick}"
        );
        assert!(
            !tick.contains("loop {") && !tick.contains("while true"),
            "tick_until_run_settled must not replace the cap with an unbounded loop:\n{tick}"
        );
        assert!(
            !tick.contains("0..MAX_CHAT_TICKS")
                && !tick.contains("0..=MAX_CHAT_TICKS")
                && !tick.contains("0..=max_ticks"),
            "loop bound must be the passed max_ticks, exclusive — 0..= or MAX_CHAT_TICKS \
             would overshoot the large-plan cap of 3:\n{tick}"
        );
        assert!(
            tick.contains("tick_project"),
            "the bounded loop must actually tick the project:\n{tick}"
        );
        assert!(
            !tick.contains("max_ticks +")
                && !tick.contains("max_ticks *")
                && !tick.contains("max_ticks.max")
                && !tick.contains("max_ticks +=")
                && !tick.contains("MAX_CHAT_TICKS_LARGE +")
                && !tick.contains("MAX_CHAT_TICKS +"),
            "must not grow the in-request budget past the caller cap:\n{tick}"
        );

        let main = include_str!("main.rs");
        let lib = include_str!("lib.rs");
        assert!(
            main.contains("background fleet ticker")
                && main.contains("list_open_project_ids")
                && main.contains("tick_project")
                && main.contains("tokio::spawn"),
            "binary must keep a background ticker so ticks 4+ of a large plan still drain:\n{}",
            &main[main.find("Local drain").unwrap_or(0)..]
                .chars()
                .take(800)
                .collect::<String>()
        );
        assert!(
            lib.contains("Does **not** start the background ticker")
                || lib.contains("Does not start the background ticker"),
            "HTTP app construction must not be the drain path (tests tick explicitly; \
             the binary starts the ticker):\n{lib}"
        );
    }

    /// Evaluate `accept_and_drive_plan`'s `max_ticks` branch for `task_count`.
    /// Accepts `if task_count OP N { A } else { B }` (either polarity).
    fn in_request_ticks_for_task_count(accept: &str, task_count: usize) -> usize {
        let assign = accept
            .find("max_ticks")
            .expect("accept_and_drive_plan must assign max_ticks");
        let call = accept
            .find("tick_until_run_settled")
            .expect("accept_and_drive_plan must call tick_until_run_settled");
        assert!(
            assign < call,
            "max_ticks must be chosen before tick_until_run_settled:\n{accept}"
        );
        let block = &accept[assign..call];
        let if_at = block
            .find("if task_count")
            .or_else(|| block.find("if plan.tasks.len()"))
            .unwrap_or_else(|| {
                panic!(
                    "max_ticks must branch on task_count so large plans cannot inherit 24 ticks:\n{block}"
                )
            });
        let if_src = &block[if_at..];
        let (op, threshold) = parse_task_count_cmp(if_src);
        let consts = tick_budget_consts(if_src);
        assert!(
            consts.len() >= 2,
            "max_ticks must choose between MAX_CHAT_TICKS and MAX_CHAT_TICKS_LARGE:\n{if_src}"
        );
        assert!(
            consts.contains(&MAX_CHAT_TICKS) && consts.contains(&MAX_CHAT_TICKS_LARGE),
            "both budgets must appear in the branch (single-task 24, large 3):\n{if_src}"
        );
        let take_then = match op {
            "<=" => task_count <= threshold,
            "<" => task_count < threshold,
            ">=" => task_count >= threshold,
            ">" => task_count > threshold,
            "==" => task_count == threshold,
            "!=" => task_count != threshold,
            other => panic!("unsupported task_count comparison {other:?} in:\n{if_src}"),
        };
        if take_then {
            consts[0]
        } else {
            consts[1]
        }
    }

    fn parse_task_count_cmp(if_src: &str) -> (&'static str, usize) {
        let after = if let Some(i) = if_src.find("task_count") {
            &if_src[i + "task_count".len()..]
        } else if let Some(i) = if_src.find("plan.tasks.len()") {
            &if_src[i + "plan.tasks.len()".len()..]
        } else {
            panic!("missing task_count in:\n{if_src}");
        };
        let trimmed = after.trim_start();
        let (op, rest) = if let Some(r) = trimmed.strip_prefix("<=") {
            ("<=", r)
        } else if let Some(r) = trimmed.strip_prefix(">=") {
            (">=", r)
        } else if let Some(r) = trimmed.strip_prefix("!=") {
            ("!=", r)
        } else if let Some(r) = trimmed.strip_prefix("==") {
            ("==", r)
        } else if let Some(r) = trimmed.strip_prefix('<') {
            ("<", r)
        } else if let Some(r) = trimmed.strip_prefix('>') {
            (">", r)
        } else {
            panic!("max_ticks must compare task_count to a cutoff:\n{if_src}");
        };
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let n: usize = digits.parse().unwrap_or_else(|_| {
            panic!("task_count comparison needs a numeric cutoff (e.g. 8):\n{if_src}")
        });
        (op, n)
    }

    fn tick_budget_consts(s: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find("MAX_CHAT_TICKS") {
            if rest[i..].starts_with("MAX_CHAT_TICKS_LARGE") {
                out.push(MAX_CHAT_TICKS_LARGE);
                rest = &rest[i + "MAX_CHAT_TICKS_LARGE".len()..];
            } else {
                out.push(MAX_CHAT_TICKS);
                rest = &rest[i + "MAX_CHAT_TICKS".len()..];
            }
        }
        out
    }

    /// Docs §6.2/§7: model tool calls repeat up to a finite cap
    /// (`MAX_TOOL_ROUNDS`). A tool-only model must not recurse unbounded.
    /// Extra fail-rounds (Grok/Ollama) stay bounded by `EXTRA_ROUNDS_ON_FAIL`.
    #[test]
    fn tool_rounds_cap_is_finite_and_used_by_every_loop() {
        assert!(
            (1..=32).contains(&MAX_TOOL_ROUNDS),
            "MAX_TOOL_ROUNDS must be a small finite cap, got {MAX_TOOL_ROUNDS}"
        );

        let extra = crate::retry::EXTRA_ROUNDS_ON_FAIL;
        assert!(
            extra <= 8,
            "EXTRA_ROUNDS_ON_FAIL must stay a small add-on, got {extra}"
        );
        assert!(
            MAX_TOOL_ROUNDS.saturating_add(extra) < 64,
            "even with fail extras the loop must stay finite"
        );

        let providers = production_src_of(include_str!("providers.rs"));
        let grok = production_src_of(include_str!("grok.rs"));
        let chat = include_str!("chat.rs");

        for (label, body) in [
            (
                "openai_chat_with_tools",
                fn_src(providers, "pub async fn openai_chat_with_tools"),
            ),
            (
                "claude_chat_with_tools",
                fn_src(providers, "pub async fn claude_chat_with_tools"),
            ),
            (
                "grok::chat_with_tools",
                fn_src(grok, "pub async fn chat_with_tools"),
            ),
            (
                "ollama::chat_with_tools",
                fn_src(chat, "async fn chat_with_tools("),
            ),
        ] {
            assert_tool_loop_stops_at_max_rounds(label, body);
        }

        let persist = fn_src(chat, "async fn persist_user_and_assistant");
        let text_at = persist
            .rfind("assistant_text")
            .expect("persist_user_and_assistant must write assistant_text");
        let insert_at = persist[..text_at]
            .rfind("insert_message")
            .expect("assistant_text must be passed to insert_message, not dropped");
        let window = &persist[insert_at..text_at];
        assert!(
            window.contains("\"assistant\""),
            "insert_message must persist role assistant:\n{window}"
        );
        assert!(
            !gated_on_empty_content(persist),
            "capped tool-only turns still persist the assistant message:\n{persist}"
        );

        for (label, sig, loop_call) in [
            (
                "chat_complete_openai",
                "async fn chat_complete_openai(",
                "openai_chat_with_tools",
            ),
            (
                "chat_complete_claude",
                "async fn chat_complete_claude(",
                "claude_chat_with_tools",
            ),
            (
                "chat_complete_grok",
                "async fn chat_complete_grok(",
                "chat_with_tools",
            ),
            (
                "chat_complete_ollama",
                "async fn chat_complete_ollama(",
                "chat_with_tools",
            ),
        ] {
            let body = fn_src(chat, sig);
            let loop_at = body
                .find(loop_call)
                .unwrap_or_else(|| panic!("{label} must run the tool loop ({loop_call}):\n{body}"));
            let persist_at = body.find("persist_user_and_assistant").unwrap_or_else(|| {
                panic!("{label} must persist the assistant turn after the tool loop:\n{body}")
            });
            assert!(
                loop_at < persist_at,
                "{label} must persist after the tool loop returns, not skip the assistant turn:\n{body}"
            );
            let between = &body[loop_at..persist_at];
            assert!(
                !between.contains("if content.is_empty()")
                    && !between.contains("if actions.is_empty()")
                    && !between.contains("if content.trim().is_empty()"),
                "{label} must persist a tool-only / cap-hit turn, not only a text reply:\n{between}"
            );
        }
    }

    fn production_src_of(src: &'static str) -> &'static str {
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn assert_tool_loop_stops_at_max_rounds(label: &str, body: &str) {
        assert!(
            body.contains("MAX_TOOL_ROUNDS"),
            "{label} must bound the loop with MAX_TOOL_ROUNDS:\n{body}"
        );
        assert!(
            body.contains("let max_rounds = MAX_TOOL_ROUNDS")
                || body.contains("let mut max_rounds = MAX_TOOL_ROUNDS"),
            "{label} must start max_rounds at MAX_TOOL_ROUNDS:\n{body}"
        );
        assert!(
            body.contains("while round < max_rounds"),
            "{label} must stop at max_rounds (tool-only must not recurse unbounded):\n{body}"
        );
        assert!(
            !body.contains("loop {") && !body.contains("while true"),
            "{label} must not replace the cap with an unbounded loop:\n{body}"
        );
        let sig_end = body.find('{').unwrap_or(0);
        let after_sig = &body[sig_end..];
        let short = label.rsplit("::").next().unwrap_or(label);
        let short_call = format!("{short}(");
        assert!(
            !after_sig.contains(&short_call),
            "{label} must not recurse into itself:\n{body}"
        );
        if body.contains("let mut max_rounds") {
            assert!(
                body.contains("EXTRA_ROUNDS_ON_FAIL")
                    && body.contains("extra < ")
                    && body.contains("max_rounds += 1"),
                "{label} may grow max_rounds only by EXTRA_ROUNDS_ON_FAIL, one at a time:\n{body}"
            );
        } else {
            assert!(
                !body.contains("max_rounds +="),
                "{label} must keep max_rounds pinned at MAX_TOOL_ROUNDS:\n{body}"
            );
        }
        assert!(
            body.contains(", false)")
                || body.contains("false,")
                || body.contains("with_tools: false"),
            "{label} must make one last no-tools call after the cap, not keep calling tools:\n{body}"
        );
        assert!(
            body.contains("Reached tool round limit"),
            "{label} must surface the cap so the assistant turn can persist:\n{body}"
        );
    }

    fn gated_on_empty_content(src: &str) -> bool {
        src.contains("if assistant_text.is_empty()")
            || src.contains("if assistant_text.trim().is_empty()")
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
        assert!(!names.contains(&"github_commit_files"));
        assert!(!names.contains(&"github_create_pr"));
        assert!(names.contains(&"list_sandboxes"));
        assert!(names.contains(&"list_integrations"));
        assert!(names.contains(&"get_integration"));
        assert!(names.contains(&"list_agent_personas"));
        assert!(names.contains(&"configure_fleet"));
        assert!(names.contains(&"read_team_chat"));
        assert!(names.contains(&"send_team_message"));
        assert!(names.contains(&"monitor_tickets"));
        assert!(names.contains(&"list_tickets"));
        assert!(names.contains(&"finish_ticket"));
        assert!(names.contains(&"start_case"));
        assert!(names.contains(&"list_cases"));
        assert!(names.contains(&"get_case"));
        assert!(names.contains(&"observe_case"));
        assert!(names.contains(&"cancel_case"));
    }

    #[test]
    fn coerce_fills_notion_text_from_slack_history() {
        let prior = vec![json!({
            "ok": true,
            "provider": "slack",
            "operation": "get_channel_history",
            "data": {
                "channel": "C0BPJEM3ZQD",
                "items": [
                    { "user": "U1", "text": "ship it" },
                    { "user": "U2", "text": "lgtm" }
                ]
            }
        })];
        let fixed = coerce_integration_args(
            "notion",
            "create_page",
            &json!({ "parent_page_id": "abc", "title": "dump" }),
            &prior,
        )
        .expect("coerced");
        let text = fixed["text"].as_str().unwrap();
        assert!(text.contains("ship it"), "{text}");
        assert!(text.contains("lgtm"), "{text}");
    }

    #[test]
    fn coerce_resolves_hash_eng_from_listed_channels() {
        let prior = vec![json!({
            "ok": true,
            "provider": "slack",
            "operation": "list_channels",
            "data": {
                "items": [
                    { "id": "CENG", "name": "eng" },
                    { "id": "CGEN", "name": "general" }
                ]
            }
        })];
        let fixed = coerce_integration_args(
            "slack",
            "get_channel_history",
            &json!({ "channel": "#eng", "limit": 10 }),
            &prior,
        )
        .expect("coerced");
        assert_eq!(fixed["channel"], "CENG");
    }

    #[test]
    fn retry_hint_names_required_text() {
        let h = retry_hint("notion", "append_blocks");
        assert_eq!(h["should_retry"], true);
        assert!(h["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "text"));
    }

    #[test]
    fn continue_rewrite_does_not_inject_ship_playbook() {
        let launched = vec![ChatAction {
            tool: "launch_agent_plan".into(),
            ok: true,
            summary: "ok".into(),
            run_id: Some("r".into()),
            sandbox_ids: None,
            detail: None,
        }];
        let rewritten =
            rewrite_if_asked_to_continue("Say continue and I'll commit_files.", &launched)
                .expect("rewrite");
        assert!(
            rewritten.contains("Fleet `r` is live") || rewritten.contains("is live on this screen"),
            "{rewritten}"
        );
        let lower = rewritten.to_ascii_lowercase();
        assert!(!lower.contains("say continue"));
        assert!(!lower.contains("implement → review → ship"));
    }

    #[test]
    fn chat_plan_is_not_rewritten_into_a_pr_pipeline() {
        let plan = build_chat_plan(
            Uuid::nil(),
            "Calculator",
            "Implement the calculator and push to GitHub then write Notion.",
            "coding",
        );
        let ids: Vec<_> = plan.tasks.iter().map(|t| t.plan_task_id.as_str()).collect();
        assert_eq!(ids, vec!["t1"]);
        assert_eq!(plan.tasks[0].worker_profile, "coding");
    }

    #[test]
    fn connector_catalog_lists_every_provider_and_ops() {
        let md = connector_catalog_markdown(&["slack".into()]);
        for p in supported_providers() {
            assert!(md.contains(&format!("- {p} [")), "missing {p} in {md}");
        }
        assert!(md.contains("slack [connected]"));
        assert!(md.contains("github [not connected]"));
        assert!(md.contains("post_message"));
        assert!(md.contains("commit_files"));
        assert!(md.contains("create_page"));
    }

    /// Docs §7.2 `github_create_pr`: open a PR after commits.
    /// Docs §7.3: github allowlist includes `create_pr`. Unknown op /
    /// disconnected / decrypt failure → tool `ok: false`. Results are
    /// sanitized (no token echo). Workspace scoped.
    ///
    /// Product may expose the docs name as a dedicated tool or as
    /// `integration_query` + allowlisted `create_pr`. Dropping `create_pr`
    /// from the github allowlist or match arm, skipping sanitize, echoing
    /// the bearer, or letting args retarget another workspace fails this test.
    #[tokio::test]
    async fn github_create_pr_is_allowlisted_and_does_not_echo_the_token() {
        // Hardcoded from docs §7.3 — do not read OPERATIONS / operations_for
        // when building this list.
        const DOC_GITHUB_OPS: &[&str] = &[
            "list_repos",
            "list_issues",
            "get_issue",
            "list_prs",
            "get_pr",
            "get_file",
            "list_commits",
            "put_file",
            "commit_files",
            "create_branch",
            "create_pr",
            "create_issue",
            "add_comment",
        ];
        let ops = operations_for("github");
        for op in DOC_GITHUB_OPS {
            assert!(
                ops.contains(op),
                "docs §7.3 github allowlist must include `{op}` (github_create_pr is create_pr)"
            );
        }
        assert!(
            ops.contains(&"create_pr"),
            "docs §7.2 github_create_pr must stay an allowlisted github op"
        );

        let catalog = connector_catalog_markdown(&["github".into()]);
        assert!(
            catalog.contains("create_pr"),
            "model catalog must advertise create_pr (github_create_pr after commits):\n{catalog}"
        );
        let caps = provider_capabilities("github");
        assert!(
            caps.contains("create_pr"),
            "get_integration capabilities must list create_pr: {caps}"
        );
        assert!(
            catalog.contains("commit_files") && caps.contains("commit_files"),
            "docs §7.2 github_create_pr is the after-commits step; commit_files must stay advertised:\n{catalog}\n{caps}"
        );

        let tools = tool_definitions();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(
            names.contains(&"integration_query"),
            "docs §7.2 github_create_pr runs through integration_query: {names:?}"
        );

        let session = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let create_pr_args = json!({
            "provider": "github",
            "operation": "create_pr",
            "workspace_id": foreign.to_string(),
            "args": {
                "owner": "acme",
                "repo": "app",
                "title": "add README",
                "head": "agent-fleet/changes",
                "workspace_id": foreign.to_string(),
            },
        });
        let err = require_session_workspace(session, &create_pr_args)
            .expect_err("github_create_pr must stay workspace-scoped");
        assert_forbidden(err);
        require_session_workspace(
            session,
            &json!({
                "provider": "github",
                "operation": "create_pr",
                "args": {
                    "owner": "acme",
                    "repo": "app",
                    "title": "add README",
                    "head": "agent-fleet/changes",
                },
            }),
        )
        .expect("omitted workspace_id uses the chat session");

        const LEAK: &str = "gho_CREATE_PR_MUST_NOT_ECHO_7f2a9c";
        let create_pr_payload = json!({
            "owner": "acme",
            "repo": "app",
            "title": "add README",
            "head": "agent-fleet/changes",
            "access_token": LEAK,
            "token": LEAK,
        });

        // Wired, not only listed. Missing title is local bad_args — never
        // unknown_op, never a GitHub CONNECT, never echo the bearer.
        let missing_sink = CreatePrSink::bind();
        let missing = proxy::dispatch(
            "github",
            &missing_sink.http,
            LEAK,
            "create_pr",
            &json!({ "access_token": LEAK, "token": LEAK }),
            &json!({}),
        )
        .await;
        assert!(
            !missing.ok,
            "create_pr bad args must be ok:false: {missing:?}"
        );
        assert_eq!(
            missing.error_code.as_deref(),
            Some("bad_args"),
            "create_pr must be dispatched (bad_args), not unknown_op: {missing:?}"
        );
        assert_eq!(
            missing_sink.hits(),
            0,
            "create_pr bad_args must not hit GitHub (sink saw {}): {missing:?}",
            missing_sink.hits()
        );
        let missing_s = serde_json::to_string(&missing).expect("serialize missing");
        assert!(
            !missing_s.contains(LEAK),
            "create_pr bad_args must not echo the token: {missing_s}"
        );

        // Valid create_pr after commits hits outbound HTTP. A local sink is
        // not GitHub — ok:false, not unknown_op / bad_args, bearer stays out.
        let sink = CreatePrSink::bind();
        let forwarded = proxy::dispatch(
            "github",
            &sink.http,
            LEAK,
            "create_pr",
            &create_pr_payload,
            &json!({}),
        )
        .await;
        assert_eq!(forwarded.provider, "github");
        assert_eq!(forwarded.operation, "create_pr");
        assert!(
            !forwarded.ok,
            "sink is not GitHub — create_pr must be ok:false: {forwarded:?}"
        );
        assert_ne!(
            forwarded.error_code.as_deref(),
            Some("unknown_op"),
            "docs §7.2 github_create_pr must stay wired create_pr, not unknown_op: {forwarded:?}"
        );
        assert_ne!(
            forwarded.error_code.as_deref(),
            Some("bad_args"),
            "valid create_pr args must reach HTTP, not fail locally: {forwarded:?}"
        );
        assert!(
            sink.hits() > 0,
            "allowlisted create_pr must attempt GitHub (sink saw 0 hits): {forwarded:?}"
        );
        let forwarded_s = serde_json::to_string(&forwarded).expect("serialize forwarded");
        assert!(
            !forwarded_s.contains(LEAK),
            "create_pr upstream result must not echo the token: {forwarded_s}"
        );

        // Unknown op is fail-closed locally — never forwarded as create_pr,
        // never a GitHub CONNECT, never echoes a token stuffed in args.
        let unknown_sink = CreatePrSink::bind();
        let unknown_dispatch = proxy::dispatch(
            "github",
            &unknown_sink.http,
            LEAK,
            "dump_access_token",
            &create_pr_payload,
            &json!({}),
        )
        .await;
        assert!(
            !unknown_dispatch.ok,
            "unknown github op must be ok:false: {unknown_dispatch:?}"
        );
        assert_eq!(
            unknown_dispatch.error_code.as_deref(),
            Some("unknown_op"),
            "unknown github op must not be forwarded as create_pr: {unknown_dispatch:?}"
        );
        assert_eq!(
            unknown_sink.hits(),
            0,
            "unknown op must not hit GitHub (sink saw {}): {unknown_dispatch:?}",
            unknown_sink.hits()
        );
        let unknown_dispatch_s =
            serde_json::to_string(&unknown_dispatch).expect("serialize unknown dispatch");
        assert!(
            !unknown_dispatch_s.contains(LEAK),
            "unknown_op dispatch must not echo the token: {unknown_dispatch_s}"
        );

        let raw_pr = json!({
            "number": 12,
            "title": "add README",
            "html_url": "https://github.com/acme/app/pull/12",
            "access_token": LEAK,
            "refresh_token": "refresh_CREATE_PR_MUST_NOT_ECHO",
            "token": LEAK,
        });
        let clean = proxy::sanitize_value(&raw_pr);
        assert!(
            !proxy::contains_secret_keys(&clean),
            "create_pr payload must be sanitized: {clean}"
        );
        let clean_s = serde_json::to_string(&clean).expect("serialize sanitized pr");
        assert!(
            !clean_s.contains(LEAK) && !clean_s.contains("refresh_CREATE_PR"),
            "sanitized create_pr data must not echo the token: {clean_s}"
        );
        assert_eq!(clean["number"], 12);
        assert_eq!(clean["html_url"], "https://github.com/acme/app/pull/12");

        // Same shape proxy_tool_result returns to the model.
        let mut pr = ProxyResult::ok_result(
            "github",
            "create_pr",
            "Opened PR #12: add README",
            raw_pr,
            Some(201),
        );
        pr.data = proxy::sanitize_value(&pr.data);
        if proxy::contains_secret_keys(&pr.data) {
            pr.data = Value::Null;
        }
        let tool_payload = json!({
            "ok": pr.ok,
            "provider": pr.provider,
            "operation": pr.operation,
            "summary": pr.summary,
            "data": pr.data,
            "error_code": pr.error_code,
            "provider_status": pr.provider_status,
        });
        let md = format_context_markdown(&[pr.clone()]);
        let pr_s = serde_json::to_string(&pr).expect("serialize proxy result");
        let tool_s = serde_json::to_string(&tool_payload).expect("serialize tool payload");
        assert!(
            !md.contains(LEAK),
            "context markdown must not echo token:\n{md}"
        );
        assert!(
            !pr_s.contains(LEAK) && !tool_s.contains(LEAK),
            "ProxyResult / tool payload for create_pr must not echo token: {pr_s} {tool_s}"
        );

        // Decrypt fail is ok:false and must not echo the sealed token.
        // invoke maps credentials_for Err → decrypt_failed with a static
        // summary (no `{e}` interpolation that could echo material).
        let seal_key =
            fleet_integrations::CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .expect("seal key");
        let wrong_key =
            fleet_integrations::CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .expect("wrong key");
        let sealed = seal_key
            .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
            .expect("seal token");
        assert!(
            !serde_json::to_string(&sealed)
                .expect("sealed")
                .contains(LEAK),
            "sealed credentials must not contain the plaintext token"
        );
        let decrypt_err = wrong_key
            .decrypt(&sealed)
            .expect_err("wrong INTEGRATIONS_ENCRYPTION_KEY must fail")
            .to_string();
        assert!(
            !decrypt_err.contains(LEAK),
            "decrypt failure must not echo the token: {decrypt_err}"
        );

        // Unknown op returns before any DB / GitHub call. A token in args
        // must not appear on the result. create_pr itself is allowlisted
        // so this is not a tautology that every github op is unknown_op.
        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool (unknown_op must not acquire)");
        // Closed pool fails acquire immediately — create_pr is allowlisted so
        // it reaches find_connected; unknown_op must not.
        pool.close().await;
        let key =
            fleet_integrations::CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
                .expect("test credentials key");
        let unknown = proxy::invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: session,
                provider: "github".into(),
                operation: "dump_access_token".into(),
                args: json!({
                    "access_token": LEAK,
                    "token": LEAK,
                    "owner": "acme",
                    "repo": "app",
                }),
                caller: ProxyCaller::Chat {
                    user_id: Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(
            !unknown.ok,
            "unknown github op must be ok:false: {unknown:?}"
        );
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("unknown_op"),
            "unknown github op must be unknown_op, not forwarded: {unknown:?}"
        );
        let unknown_s = serde_json::to_string(&unknown).expect("serialize unknown_op");
        assert!(
            !unknown_s.contains(LEAK),
            "unknown_op result must not echo the token: {unknown_s}"
        );

        // Allowlisted github_create_pr / create_pr must not be treated as
        // unknown_op. Dead pool → ok:false before GitHub; args token stays out.
        let create_pr = proxy::invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: session,
                provider: "github".into(),
                operation: "create_pr".into(),
                args: json!({
                    "owner": "acme",
                    "repo": "app",
                    "title": "add README",
                    "head": "agent-fleet/changes",
                    "access_token": LEAK,
                    "workspace_id": foreign.to_string(),
                }),
                caller: ProxyCaller::Chat {
                    user_id: Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(
            !create_pr.ok,
            "create_pr without a reachable connection must be ok:false: {create_pr:?}"
        );
        assert_ne!(
            create_pr.error_code.as_deref(),
            Some("unknown_op"),
            "docs §7.2 github_create_pr is allowlisted create_pr, not unknown_op: {create_pr:?}"
        );
        let create_s = serde_json::to_string(&create_pr).expect("serialize create_pr");
        assert!(
            !create_s.contains(LEAK),
            "create_pr result must not echo the token: {create_s}"
        );

        let github_tools =
            include_str!("../../fleet-integrations/src/proxy/apps/github/tools/mod.rs");
        let github_tools = match github_tools.find("#[cfg(test)]") {
            Some(i) => &github_tools[..i],
            None => github_tools,
        };
        assert!(
            github_tools.contains("\"create_pr\" => writes::create_pr"),
            "docs §7.2 github_create_pr must be wired to writes::create_pr:\n{github_tools}"
        );

        let src = production_src();
        let exec = fn_src(src, "pub async fn execute_tool");
        assert!(
            exec.contains("\"integration_query\" => integration_query("),
            "github_create_pr / create_pr must run through integration_query:\n{exec}"
        );
        assert!(
            exec.contains("require_session_workspace"),
            "execute_tool must workspace-scope integration_query:\n{exec}"
        );
        if let Some(at) = exec.find("\"github_create_pr\"") {
            let window: String = exec[at..].lines().take(24).collect::<Vec<_>>().join("\n");
            assert!(
                window.contains("proxy_tool_result")
                    || window.contains("integration_query")
                    || window.contains("proxy::invoke"),
                "dedicated github_create_pr must use the sanitized proxy:\n{window}"
            );
        }

        let iq = fn_src(src, "async fn integration_query");
        assert!(
            iq.contains("require_session_workspace"),
            "integration_query must reject a foreign workspace_id:\n{iq}"
        );
        assert!(
            iq.contains("proxy_tool_result"),
            "integration_query must dispatch via proxy_tool_result:\n{iq}"
        );

        let ptr = fn_src(src, "async fn proxy_tool_result");
        let invoke_at = ptr
            .find("proxy::invoke")
            .expect("proxy_tool_result must call proxy::invoke");
        assert!(
            !ptr.contains("access_token") && !ptr.contains("refresh_token"),
            "proxy_tool_result must not copy credentials into the tool payload:\n{ptr}"
        );
        assert!(
            ptr[invoke_at..].contains("result.data"),
            "tool payload must use the sanitized ProxyResult, not raw creds:\n{ptr}"
        );

        let proxy_src = proxy_production_src();
        let invoke = fn_src(proxy_src, "pub async fn invoke");
        let unknown_at = invoke
            .find("unknown_op")
            .expect("invoke must fail-closed on unknown op");
        let decrypt_at = invoke
            .find("decrypt_failed")
            .expect("invoke must fail-closed on decrypt failure");
        let find_at = invoke
            .find("find_connected")
            .expect("invoke loads the session workspace connection");
        let dispatch_at = invoke
            .find("apps::dispatch")
            .expect("invoke dispatches allowlisted ops only");
        assert!(
            unknown_at < find_at && unknown_at < dispatch_at,
            "unknown op must return before connection load / GitHub:\n{invoke}"
        );
        assert!(
            decrypt_at < dispatch_at,
            "decrypt failure must return before GitHub:\n{invoke}"
        );
        let decrypt_arm: String = invoke[decrypt_at..]
            .lines()
            .take(8)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            invoke.contains("ProxyResult::err")
                && decrypt_arm.contains("decrypt_failed")
                && decrypt_arm.contains("failed to decrypt credentials"),
            "decrypt failure must be ok:false decrypt_failed:\n{decrypt_arm}"
        );
        assert!(
            !decrypt_arm.contains("{e}") && !decrypt_arm.contains("{e:?}"),
            "decrypt_failed summary must be a static string (interpolating the decrypt error would echo material):\n{decrypt_arm}"
        );
        assert!(
            invoke.contains("sanitize_value") && invoke.contains("contains_secret_keys"),
            "create_pr (and every op) results must be sanitized:\n{invoke}"
        );
        assert!(
            invoke.contains("drop(access_token)"),
            "decrypted token must not live on the ProxyResult:\n{invoke}"
        );

        // Lookup is ProxyRequest.workspace_id (inlined or via a helper).
        // `&req.args` is only legal as the dispatch payload, after the
        // connection is already loaded from the session workspace.
        let before_dispatch = &invoke[..dispatch_at];
        assert!(
            before_dispatch.contains("find_connected")
                && (before_dispatch.contains("req.workspace_id")
                    || proxy_src.contains("fn connection_scope")),
            "lookup must stay on the session workspace:\n{before_dispatch}"
        );
        assert!(
            !before_dispatch.contains("req.args") && !before_dispatch.contains("connection_id"),
            "args.connection_id / args.workspace_id must not retarget lookup:\n{before_dispatch}"
        );
        if proxy_src.contains("fn connection_scope") {
            let scope = fn_src(proxy_src, "fn connection_scope");
            assert!(
                scope.contains("req.workspace_id"),
                "lookup must stay on the session workspace:\n{scope}"
            );
            assert!(
                !scope.contains("req.args") && !scope.contains("connection_id"),
                "args.connection_id / args.workspace_id must not retarget lookup:\n{scope}"
            );
        }
    }

    /// Docs §7.3: unknown op / disconnected / decrypt failure → tool
    /// `ok: false`. `integration_query` decrypts on the control plane and
    /// the returned body must not contain `access_token`.
    #[tokio::test]
    async fn integration_query_decrypts_server_side_and_sanitizes_tokens() {
        const LEAK: &str = "gho_IQ_CHAT_SANITIZE_MUST_NOT_ECHO_99ab";

        fn assert_no_token_echo(label: &str, body: &str) {
            assert!(
                !body.contains("access_token") && !body.contains(LEAK),
                "{label} must not contain access_token / the decrypted token: {body}"
            );
            assert!(
                !body.contains("refresh_token") && !body.contains("client_secret"),
                "{label} must not echo other credential keys: {body}"
            );
        }

        fn tool_body(result: &ProxyResult) -> Value {
            json!({
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
                "operations": operations_for(&result.provider),
            })
        }

        let tools = tool_definitions();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(
            names.contains(&"integration_query"),
            "docs §7.2/§7.3 integration_query must stay a chat tool: {names:?}"
        );
        let iq_def = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == "integration_query")
            .expect("integration_query definition");
        let desc = iq_def["function"]["description"].as_str().unwrap_or("");
        assert!(
            desc.to_ascii_lowercase().contains("control plane")
                && desc.to_ascii_lowercase().contains("token"),
            "integration_query must tell the model tokens stay on the control plane: {desc}"
        );

        let stuffed = json!({
            "provider": "github",
            "operation": "raw_http",
            "args": {
                "access_token": LEAK,
                "refresh_token": LEAK,
                "token": LEAK,
                "owner": "acme",
                "repo": "app",
            },
        });
        let session = Uuid::new_v4();
        require_session_workspace(session, &stuffed)
            .expect("omitted workspace_id uses the chat session");

        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool");
        pool.close().await;
        let key =
            fleet_integrations::CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
                .expect("test credentials key");

        let unknown = proxy::invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: session,
                provider: "github".into(),
                operation: "raw_http".into(),
                args: stuffed["args"].clone(),
                caller: ProxyCaller::Chat {
                    user_id: Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(!unknown.ok, "unknown op must be ok:false: {unknown:?}");
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("unknown_op"),
            "docs §7.3 unknown op → ok:false unknown_op: {unknown:?}"
        );
        let unknown_content = tool_body(&unknown);
        let unknown_action = ChatAction {
            tool: "integration_query".into(),
            ok: unknown.ok,
            summary: unknown.summary.clone(),
            run_id: None,
            sandbox_ids: None,
            detail: Some(unknown_content.clone()),
        };
        let unknown_s = serde_json::to_string(&unknown_content).expect("unknown content");
        let unknown_act_s = serde_json::to_string(&unknown_action).expect("unknown action");
        assert_no_token_echo("unknown_op integration_query content", &unknown_s);
        assert_no_token_echo("unknown_op ChatAction", &unknown_act_s);

        let disconnected = proxy::invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: session,
                provider: "github".into(),
                operation: "list_repos".into(),
                args: stuffed["args"].clone(),
                caller: ProxyCaller::Chat {
                    user_id: Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(
            !disconnected.ok,
            "disconnected list_repos must be ok:false: {disconnected:?}"
        );
        assert_ne!(
            disconnected.error_code.as_deref(),
            Some("unknown_op"),
            "allowlisted list_repos must not be treated as unknown_op: {disconnected:?}"
        );
        assert!(
            matches!(
                disconnected.error_code.as_deref(),
                Some("not_connected") | Some("internal")
            ),
            "disconnected is not_connected (or internal if the pool is dead): {disconnected:?}"
        );
        let disconnected_s =
            serde_json::to_string(&tool_body(&disconnected)).expect("disconnected content");
        assert_no_token_echo("disconnected integration_query body", &disconnected_s);

        let seal_key =
            fleet_integrations::CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .expect("seal key");
        let wrong_key =
            fleet_integrations::CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .expect("wrong key");
        let sealed = seal_key
            .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
            .expect("seal");
        assert!(
            !serde_json::to_string(&sealed)
                .expect("sealed")
                .contains(LEAK),
            "sealed credentials must not contain the plaintext token"
        );
        let decrypt_err = wrong_key
            .decrypt(&sealed)
            .expect_err("wrong INTEGRATIONS_ENCRYPTION_KEY must fail")
            .to_string();
        assert!(
            !decrypt_err.contains(LEAK),
            "decrypt failure must not echo the token: {decrypt_err}"
        );
        let decrypt_failed = ProxyResult::err(
            "github",
            "list_repos",
            "decrypt_failed",
            "failed to decrypt credentials (check INTEGRATIONS_ENCRYPTION_KEY)",
            None,
        );
        assert!(!decrypt_failed.ok);
        let decrypt_s =
            serde_json::to_string(&tool_body(&decrypt_failed)).expect("decrypt_failed content");
        assert_no_token_echo("decrypt_failed integration_query body", &decrypt_s);

        let dirty = json!({
            "full_name": "acme/app",
            "access_token": LEAK,
            "refresh_token": "refresh_IQ_CHAT_MUST_NOT_ECHO",
        });
        let mut ok = ProxyResult::ok_result("github", "list_repos", "ok", dirty, Some(200));
        ok.data = proxy::sanitize_value(&ok.data);
        if proxy::contains_secret_keys(&ok.data) {
            ok.data = Value::Null;
        }
        let ok_s = serde_json::to_string(&tool_body(&ok)).expect("ok content");
        assert_no_token_echo("successful integration_query body", &ok_s);
        assert_eq!(ok.data["full_name"], "acme/app");

        let retry = retry_hint("github", "list_repos");
        let retry_s = serde_json::to_string(&retry).expect("retry hint");
        assert_no_token_echo("integration_query retry.example", &retry_s);

        let src = production_src();
        let exec = fn_src(src, "pub async fn execute_tool");
        assert!(
            exec.contains("\"integration_query\" => integration_query("),
            "execute_tool must run integration_query:\n{exec}"
        );
        let iq = fn_src(src, "async fn integration_query");
        assert!(
            iq.contains("require_session_workspace") && iq.contains("proxy_tool_result"),
            "integration_query must stay workspace-scoped and go through the proxy:\n{iq}"
        );
        assert!(
            !iq.contains("credentials_for")
                && !iq.contains("access_token")
                && !iq.contains("refresh_token"),
            "integration_query must not decrypt or copy tokens:\n{iq}"
        );
        let ptr = fn_src(src, "async fn proxy_tool_result");
        assert!(
            ptr.contains("proxy::invoke")
                && ptr.contains("result.data")
                && ptr.contains("decrypt_on")
                && ptr.contains("control_plane")
                && !ptr.contains("access_token")
                && !ptr.contains("credentials_for"),
            "proxy_tool_result decrypts via invoke on the control plane; body is sanitized:\n{ptr}"
        );

        let proxy_src = proxy_production_src();
        let invoke = fn_src(proxy_src, "pub async fn invoke");
        let unknown_at = invoke.find("unknown_op").expect("unknown_op");
        let decrypt_at = invoke.find("decrypt_failed").expect("decrypt_failed");
        let not_connected_at = invoke.find("not_connected").expect("not_connected");
        let dispatch_at = invoke.find("apps::dispatch").expect("dispatch");
        assert!(
            unknown_at < dispatch_at && decrypt_at < dispatch_at && not_connected_at < dispatch_at,
            "docs §7.3 error paths must be ok:false before provider HTTP:\n{invoke}"
        );
        assert!(
            invoke.contains("credentials_for")
                && invoke.contains("drop(access_token)")
                && invoke.contains("sanitize_value(&result.data)"),
            "invoke must decrypt server-side, drop the token, and sanitize:\n{invoke}"
        );
        let decrypt_arm: String = invoke[decrypt_at..]
            .lines()
            .take(8)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            decrypt_arm.contains("failed to decrypt credentials")
                && !decrypt_arm.contains("{e}")
                && !decrypt_arm.contains("{e:?}"),
            "decrypt_failed summary must be static (no error interpolation):\n{decrypt_arm}"
        );
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

    /// HTTP client that can only reach a local sink. Any connect is an
    /// outbound GitHub attempt (`api.github.com` is HTTPS — CONNECT hits us).
    struct CreatePrSink {
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        http: reqwest::Client,
    }

    impl Drop for CreatePrSink {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl CreatePrSink {
        fn bind() -> Self {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            use std::sync::Arc;
            use std::thread;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind create_pr sink");
            listener.set_nonblocking(true).expect("nonblocking sink");
            let addr = listener.local_addr().expect("sink addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
            thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            hits_t.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                            let mut buf = [0u8; 128];
                            let _ = stream.read(&mut buf);
                            let _ = stream.write_all(
                                b"HTTP/1.1 503 sink\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            );
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            let http = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{addr}")).expect("proxy"))
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client");
            Self { hits, stop, http }
        }

        fn hits(&self) -> usize {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn production_src() -> &'static str {
        let src = include_str!("chat_tools.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn proxy_production_src() -> &'static str {
        let src = include_str!("../../fleet-integrations/src/proxy/mod.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn launch_plan_task_json(i: usize) -> Value {
        json!({ "id": format!("t{i}"), "title": format!("Task {i}") })
    }

    fn launch_plan_task(id: &str, depends_on: &[&str]) -> PlanTask {
        PlanTask {
            plan_task_id: id.to_string(),
            title: format!("Task {id}"),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: depends_on.iter().map(|s| (*s).to_string()).collect(),
            worker_profile: "generalist".into(),
            acceptance: fleet_orchestrator::orch::default_acceptance("generalist"),
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        }
    }

    /// Docs §7.1: launch_agent_plan max 1000 tasks. A constant-only
    /// assert is not coverage — 1001 unique ids must error with no
    /// accepted plan/run; 1000 is allowed. Cycles / unknown deps are
    /// still rejected by orch validate before insert. Live execute_tool
    /// + store lock is
    /// `chat_plan_execute_tool_rejects_over_max_tasks_and_inserts_no_run`.
    #[test]
    fn chat_plan_rejects_over_max_tasks() {
        const DOC_MAX: usize = 1000;

        let at_cap: Vec<Value> = (0..DOC_MAX).map(launch_plan_task_json).collect();
        assert_eq!(at_cap.len(), 1000);
        let parsed = parse_launch_plan_args(&json!({
            "title": "at cap",
            "tasks": at_cap,
        }))
        .expect("1000 unique task ids are allowed");
        assert_eq!(parsed.tasks.len(), 1000);
        let cap_ids: std::collections::HashSet<&str> =
            parsed.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(cap_ids.len(), 1000, "1000 ids must stay unique");

        let over: Vec<Value> = (0..=DOC_MAX).map(launch_plan_task_json).collect();
        assert_eq!(over.len(), 1001);
        let over_ids: std::collections::HashSet<String> = over
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(over_ids.len(), 1001, "fixture must be 1001 unique ids");
        let err = parse_launch_plan_args(&json!({
            "title": "over cap",
            "tasks": over,
        }))
        .expect_err("1001 unique task ids must fail closed");
        let lower = err.to_ascii_lowercase();
        assert!(
            (lower.contains("too many") || lower.contains("exceed"))
                && (lower.contains("max") || lower.contains("1000")),
            "1001 must fail the task cap (not a field check): {err}"
        );

        let cap_plan = build_multi_task_plan(
            Uuid::new_v4(),
            None,
            (0..DOC_MAX)
                .map(|i| launch_plan_task(&format!("t{i}"), &[]))
                .collect(),
        );
        assert_eq!(cap_plan.tasks.len(), 1000);
        assert_eq!(cap_plan.status, WorkPlanStatus::Accepted);
        validate::validate_plan(&cap_plan).expect("1000 unique tasks must pass orch validate");

        let over_plan = build_multi_task_plan(
            Uuid::new_v4(),
            None,
            (0..=DOC_MAX)
                .map(|i| launch_plan_task(&format!("t{i}"), &[]))
                .collect(),
        );
        assert_eq!(over_plan.tasks.len(), 1001);
        assert!(
            validate::validate_plan(&over_plan).is_err(),
            "1001 unique tasks must fail orch validate so nothing can be accepted"
        );

        let cycle_args = parse_launch_plan_args(&json!({
            "tasks": [
                { "id": "a", "title": "A", "depends_on": ["b"] },
                { "id": "b", "title": "B", "depends_on": ["a"] }
            ]
        }))
        .expect("a cycle under the cap is parseable; orch validate rejects it");
        assert_eq!(cycle_args.tasks.len(), 2);
        let cycle_plan = build_multi_task_plan(
            Uuid::new_v4(),
            None,
            vec![launch_plan_task("a", &["b"]), launch_plan_task("b", &["a"])],
        );
        let cycle_err = validate::validate_plan(&cycle_plan)
            .expect_err("cycles must still be rejected by orch validate")
            .to_string()
            .to_ascii_lowercase();
        assert!(cycle_err.contains("cycle"), "{cycle_err}");

        let unknown_args = parse_launch_plan_args(&json!({
            "tasks": [
                { "id": "a", "title": "A", "depends_on": ["ghost"] }
            ]
        }))
        .expect("unknown deps under the cap are parseable; orch validate rejects them");
        assert_eq!(unknown_args.tasks.len(), 1);
        let unknown_plan = build_multi_task_plan(
            Uuid::new_v4(),
            None,
            vec![launch_plan_task("a", &["ghost"])],
        );
        let unknown_err = validate::validate_plan(&unknown_plan)
            .expect_err("unknown deps must still be rejected by orch validate")
            .to_string()
            .to_ascii_lowercase();
        assert!(
            unknown_err.contains("unknown") || unknown_err.contains("depends_on"),
            "{unknown_err}"
        );
    }

    /// Docs §16: Chat `context_from` GitHub while not connected → tool error;
    /// the model tells the user to Connect. `launch_agent_task` /
    /// `launch_agent_plan` must be `ok:false` and insert no accepted plan/run.
    /// Must not launch anyway with `Failed (not_connected)` baked into
    /// `spec_md`. Prefetched markdown must never contain
    /// `access_token` / `refresh_token`.
    #[test]
    fn context_from_while_not_connected_is_tool_error_and_inserts_no_plan() {
        const LEAK: &str = "gho_CTX_FROM_MUST_NOT_ECHO_7e3c";

        let prompt = system_prompt_with_tools();
        let prompt_l = prompt.to_ascii_lowercase();
        assert!(
            prompt.contains("Connect")
                && prompt.contains("Integrations")
                && prompt_l.contains("disconnect"),
            "docs §16: model must tell the user to Connect when context_from is disconnected: {prompt}"
        );

        let tools = tool_definitions();
        for name in ["launch_agent_task", "launch_agent_plan"] {
            let def = tools
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["function"]["name"] == name)
                .unwrap_or_else(|| panic!("{name} must stay a chat tool"));
            assert!(
                def["function"]["parameters"]["properties"]
                    .get("context_from")
                    .is_some(),
                "{name} must accept context_from: {def}"
            );
        }

        let task = parse_launch_args(&json!({
            "title": "Read GitHub issues",
            "spec": "Summarize open issues.",
            "context_from": [{
                "provider": "github",
                "operation": "list_issues",
                "args": {
                    "owner": "acme",
                    "repo": "app",
                    "access_token": LEAK,
                    "refresh_token": LEAK
                }
            }]
        }))
        .expect("context_from on launch_agent_task must parse");
        let cf = task
            .context_from
            .as_ref()
            .expect("context_from must be kept");
        assert_eq!(cf[0].provider, "github");
        assert_eq!(cf[0].operation, "list_issues");

        let plan = parse_launch_plan_args(&json!({
            "title": "Read GitHub repos",
            "context_from": [{
                "provider": "github",
                "operation": "list_repos",
                "args": { "access_token": LEAK, "refresh_token": LEAK }
            }],
            "tasks": [{ "id": "t1", "title": "Summarize repos" }]
        }))
        .expect("context_from on launch_agent_plan must parse");
        assert_eq!(
            plan.context_from.as_ref().map(|v| v[0].provider.as_str()),
            Some("github")
        );

        let mut dirty = ProxyResult::ok_result(
            "github",
            "list_issues",
            "ok",
            json!({
                "items": [{ "title": "n", "access_token": LEAK, "refresh_token": LEAK }],
                "access_token": LEAK,
                "refresh_token": LEAK,
            }),
            Some(200),
        );
        dirty.data = proxy::sanitize_value(&dirty.data);
        if proxy::contains_secret_keys(&dirty.data) {
            dirty.data = Value::Null;
        }
        let md = format_context_markdown(&[dirty]);
        assert!(
            !md.contains("access_token") && !md.contains("refresh_token") && !md.contains(LEAK),
            "prefetched markdown must never contain access_token/refresh_token: {md}"
        );

        let disconnected = ProxyResult::err(
            "github",
            "list_issues",
            "not_connected",
            "github is not connected — open Workspace → Integrations to connect it",
            None,
        );
        assert!(!disconnected.ok);
        assert_eq!(disconnected.error_code.as_deref(), Some("not_connected"));
        let failed_md = format_context_markdown(&[disconnected]);
        assert!(
            failed_md.contains("Failed (not_connected)"),
            "failed prefetch markdown is the hole if it is appended to spec_md: {failed_md}"
        );
        assert!(
            !failed_md.contains("access_token")
                && !failed_md.contains("refresh_token")
                && !failed_md.contains(LEAK),
            "failed prefetch markdown must not echo tokens: {failed_md}"
        );

        let src = production_src();
        let prefetch = fn_src(src, "async fn prefetch_context");
        assert!(
            prefetch.contains("proxy::invoke"),
            "prefetch_context must go through the control-plane proxy (tokens stay off the spec):\n{prefetch}"
        );
        assert!(
            !prefetch.contains("access_token") && !prefetch.contains("refresh_token"),
            "prefetch_context must not copy tokens into markdown:\n{prefetch}"
        );

        for (label, sig) in [
            ("launch_agent_task", "async fn launch_agent_task"),
            ("launch_agent_plan", "async fn launch_agent_plan"),
        ] {
            assert_launch_fail_closes_disconnected_context_from(label, fn_src(src, sig), prefetch);
        }

        let exec = fn_src(src, "pub async fn execute_tool");
        for name in ["launch_agent_task", "launch_agent_plan"] {
            assert!(
                exec.contains(&format!("\"{name}\" =>")),
                "execute_tool must dispatch {name} so a disconnected context_from is a tool error"
            );
        }
        assert!(
            exec.contains("ok: false"),
            "execute_tool must map launch Err to ChatAction ok:false (docs §16 tool error):\n{exec}"
        );
    }

    /// Window from `prefetch_context` up to markdown / plan build / insert.
    /// Plan-validate `return Err` after `build_*_plan` must not count.
    fn context_from_prefetch_window<'a>(body: &'a str, label: &str) -> &'a str {
        let prefetch_at = body
            .find("prefetch_context")
            .unwrap_or_else(|| panic!("{label} must prefetch context_from before insert:\n{body}"));
        let rest = &body[prefetch_at..];
        let end = [
            rest.find("format_context_markdown"),
            rest.find("build_chat_plan"),
            rest.find("build_multi_task_plan"),
            rest.find("accept_and_drive_plan"),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
        &rest[..end]
    }

    fn prefetch_uses_try(window: &str) -> bool {
        window.contains(".await?")
            || window.contains("await ?")
            || window
                .lines()
                .any(|l| l.contains("prefetch_context") && l.contains('?'))
            || (window.contains("prefetch_context")
                && window.contains("map_err")
                && window.contains('?'))
    }

    fn prefetch_fn_fail_closes_disconnected(prefetch: &str) -> bool {
        let returns_result = prefetch.contains("AppResult") || prefetch.contains("Result<");
        let checks = prefetch.contains("not_connected")
            || prefetch.contains("error_code")
            || ((prefetch.contains(".ok")
                || prefetch.contains("!r.ok")
                || prefetch.contains("!result.ok"))
                && (prefetch.contains("return Err") || prefetch.contains("AppError::")));
        returns_result && checks
    }

    fn assert_launch_fail_closes_disconnected_context_from(
        label: &str,
        body: &str,
        prefetch: &str,
    ) {
        let prefetch_at = body.find("prefetch_context").unwrap_or_else(|| {
            panic!("{label} must call prefetch_context for context_from:\n{body}")
        });
        let drive_at = body.find("accept_and_drive_plan").unwrap_or_else(|| {
            panic!("{label} must persist only via accept_and_drive_plan:\n{body}")
        });
        assert!(
            prefetch_at < drive_at,
            "{label} must prefetch context_from before insert:\n{body}"
        );
        assert!(
            !body.contains("insert_accepted_plan"),
            "{label} must not insert a plan itself:\n{body}"
        );

        let window = context_from_prefetch_window(body, label);
        let uses_try = prefetch_uses_try(window);
        let explicit = (window.contains("not_connected")
            || window.contains("error_code")
            || window.contains(".ok")
            || window.contains("Connect"))
            && (window.contains("return Err") || window.contains("AppError::"));
        assert!(
            uses_try || explicit,
            "{label} must fail-close when context_from is not_connected \
             (return Err / ? before format_context_markdown or plan build); \
             must not launch with Failed (not_connected) baked into spec_md:\n{window}"
        );
        if uses_try {
            assert!(
                prefetch_fn_fail_closes_disconnected(prefetch),
                "{label} uses ? on prefetch_context, so prefetch_context must \
                 return Err on a disconnected provider (not_connected / !ok):\n{prefetch}"
            );
        }
    }
}
