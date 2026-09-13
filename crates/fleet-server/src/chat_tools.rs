//! Workspace chat tools: fleet sandboxes + control-plane integration proxy.

use std::time::Duration;

use fleet_auth::{AppError, AppResult};
use fleet_db::repos::integrations as integ_repo;
use fleet_integrations::proxy::{
    format_context_markdown, operations_for, supported_providers, ProxyCaller, ProxyResult,
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
                "description": "Create a single-task agent run on a paired runner, tick until it finishes or queues. Prefer launch_agent_plan for multi-agent fan-out. Optional context_from prefetches a connected integration into the task spec (tokens stay on the control plane).",
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
                "description": "Start or update the workspace inbox. By default it follows every connected inbox app (GitHub, Jira, Slack, …) — newly connected apps are picked up on the next poll without restarting. Tickets auto-launch work+review; Slack/Teams get one ops task; Notion/Confluence get one analysis task. Tokens stay on the control plane.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean" },
                        "follow_connected": { "type": "boolean", "description": "true (default): watch whatever is connected now. false: only the providers allowlist." },
                        "providers": { "type": "array", "items": { "type": "string" }, "description": "Optional allowlist. Empty + follow_connected watches all connected inbox apps." },
                        "filters": { "type": "object", "description": "Per-provider filters, e.g. {\"github\":{\"repos\":[\"acme/api\"]},\"jira\":{\"jql\":\"project=ENG\"}}" },
                        "github_repos": { "type": "array", "items": { "type": "string" } },
                        "jira_jql": { "type": "string" },
                        "slack_channels": { "type": "array", "items": { "type": "string" } },
                        "auto_launch": { "type": "boolean" },
                        "launch_limit": { "type": "integer" },
                        "poll_limit": { "type": "integer" }
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
    composer_or_fallback_for(state, workspace_id, None).await
}

pub async fn composer_or_fallback_for(
    state: &AppState,
    workspace_id: Uuid,
    actor: Option<Uuid>,
) -> ChatLlmContext {
    let mut llm = if let Ok(conns) =
        fleet_db::repos::integrations::list_for_workspace(&state.db, workspace_id).await
    {
        let mut found = None;
        for c in conns {
            let d = fleet_db::IntegrationAgentDefaults::from_config(&c.config);
            if let (Some(provider), Some(model)) = (d.default_provider, d.default_model) {
                found = Some(ChatLlmContext {
                    provider,
                    model,
                    runner_id: d
                        .default_runner_id
                        .as_deref()
                        .and_then(|s| Uuid::parse_str(s).ok()),
                    effort: None,
                });
                break;
            }
        }
        found
    } else {
        None
    };
    if llm.is_none() {
        if let Ok(Some((provider, model))) =
            fleet_db::repos::chats::latest_composer_llm(&state.db, workspace_id).await
        {
            llm = Some(ChatLlmContext {
                provider,
                model,
                runner_id: None,
                effort: None,
            });
        }
    }
    if llm.is_none() && state.grok.enabled {
        llm = Some(ChatLlmContext {
            provider: "grok".into(),
            model: crate::grok::default_model(),
            runner_id: None,
            effort: None,
        });
    }
    if llm.is_none() && state.ollama.enabled {
        llm = Some(ChatLlmContext {
            provider: "ollama".into(),
            model: state.ollama.default_model.clone().unwrap_or_default(),
            runner_id: None,
            effort: None,
        });
    }
    let mut llm = llm.unwrap_or_default();
    if llm.runner_id.is_none() {
        if let Some(chosen) = fleet_orchestrator::store::select_assignable_runner_for_actor(
            &state.orch.store,
            workspace_id,
            actor,
        )
        .await
        {
            llm.runner_id = Some(chosen.id());
        } else if let Some(chosen) = fleet_orchestrator::store::select_online_runner_for_actor(
            &state.orch.store,
            workspace_id,
            actor,
        )
        .await
        {
            // Busy but paired — stamp so inbox/cases wait instead of 400.
            llm.runner_id = Some(chosen.id());
        }
    }
    llm
}

/// Fleet workers run on a paired runner CLI. OpenAI chat stays on the plane.
/// Chat launch may omit `runner_id` — the task stays ready until a host
/// appears. Inbox/cases must call [`require_taking_runner`] so a ticket is
/// not marked launched while nothing can run.
pub fn require_runner_worker_llm(llm: &ChatLlmContext) -> AppResult<()> {
    let p = llm.provider.trim().to_ascii_lowercase();
    if p.is_empty() || p == "off" || p == "none" || p == "bash" || p == "mock" {
        return Ok(());
    }
    if crate::runner_chat::runner_cli_provider(&p) {
        return Ok(());
    }
    Err(AppError::bad_request(
        "fleet workers run Grok, Claude, Gemini, OpenAI, or Ollama on a paired runner"
            .to_string(),
    ))
}

pub fn require_taking_runner(llm: &ChatLlmContext) -> AppResult<()> {
    require_runner_worker_llm(llm)?;
    let p = llm.provider.trim().to_ascii_lowercase();
    if crate::runner_chat::runner_cli_provider(&p) && llm.runner_id.is_none() {
        return Err(AppError::bad_request(
            "no paired runner taking jobs — pair a fleet-host for Grok / Claude / Gemini / OpenAI / Ollama workers"
                .to_string(),
        ));
    }
    Ok(())
}

/// Runner CLI chat has no plane tools. Detect when the user asked to start
/// a fleet agent so `POST /chat` can call `launch_agent_task` after the
/// box replies. "Say only: pong" (integrations H3) must stay false.
pub fn wants_fleet_launch(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    if t.contains("launch_agent_task") || t.contains("launch_agent_plan") {
        return true;
    }
    if launch_is_negated(&t) {
        return false;
    }
    let launchy = t.contains("launch")
        || t.contains("spin up")
        || t.contains("spawn")
        || t.contains("start an")
        || t.contains("start a ")
        || t.contains("create an");
    let target = t.contains("agent")
        || t.contains("fleet")
        || t.contains("sandbox")
        || t.contains("worker");
    launchy && target
}

fn launch_is_negated(t: &str) -> bool {
    const MARKERS: &[&str] = &[
        "don't launch",
        "dont launch",
        "do not launch",
        "don't spawn",
        "dont spawn",
        "do not spawn",
        "don't start",
        "do not start",
        "never launch",
        "never spawn",
        "without launching",
        "without spawn",
        "don't spin up",
        "do not spin up",
        "don't create an agent",
        "do not create an agent",
    ];
    MARKERS.iter().any(|m| t.contains(m))
}

/// Reject a tool `workspace_id` that is not this chat's workspace.
pub fn require_session_workspace(session: Uuid, arguments: &Value) -> AppResult<()> {
    let Some(raw) = arguments.get("workspace_id") else {
        return Ok(());
    };
    if raw.is_null() {
        return Ok(());
    }
    let parsed = raw
        .as_str()
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .or_else(|| serde_json::from_value::<Uuid>(raw.clone()).ok());
    match parsed {
        Some(id) if id == session => Ok(()),
        _ => Err(AppError::forbidden("workspace_id does not match this chat")),
    }
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
                    gitlab_projects: None,
                    linear_teams: None,
                    trello_boards: None,
                    auto_launch: None,
                    follow_connected: None,
                    filters: None,
                    launch_limit: None,
                    poll_limit: None,
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
                        create_if_missing: None,
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
            if let Some(id) = harvest_slack_channel_named(prior, ch)
                .or_else(|| harvest_slack_channel(prior))
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

pub(crate) fn stamp_plan_llm(plan: &mut WorkPlanBody, llm: &ChatLlmContext) {
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
) -> Vec<ProxyResult> {
    let mut out = Vec::new();
    for item in context_from.iter().take(5) {
        let provider = item.provider.trim().to_lowercase();
        let operation = item.operation.trim().to_lowercase();
        let args = item.args.clone().unwrap_or_else(|| json!({}));
        let result = crate::workforce::gated_invoke(
            state,
            workspace_id,
            ProxyCaller::Chat { user_id },
            &provider,
            &operation,
            args,
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

async fn proxy_tool_result(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    tool_name: &str,
    provider: &str,
    operation: &str,
    args: Value,
) -> AppResult<(Value, ChatAction)> {
    let result = crate::workforce::gated_invoke(
        state,
        workspace_id,
        ProxyCaller::Chat { user_id },
        provider,
        operation,
        args,
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

    for i in 0..max_ticks {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
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
        // The plane does not complete work. Stop once materialize/assign
        // has nothing more to do this request — inbox arrives from a host.
        if i >= 1
            && report.spawned == 0
            && report.completed_via_runtime == 0
            && report.inbox_applied == 0
        {
            break;
        }
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
        let id = Uuid::parse_str(raw).map_err(|_| AppError::bad_request("run_id must be a UUID"))?;
        return state
            .orch
            .store
            .get_run(workspace_id, id)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .map(|r| r.id)
            .ok_or_else(|| AppError::not_found("run"));
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
            policy_md: Some(policy_md.clone()),
            stage,
            context,
            wake_in_s,
        },
    )
    .await?;
    if let Err(e) = crate::workforce::attach_case_to_goal(
        state,
        workspace_id,
        Some(user_id),
        &title,
        &policy_md,
        case.id,
    )
    .await
    {
        tracing::warn!(%workspace_id, case_id = %case.id, error = %e, "attach goal to case failed");
    }
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
        .unwrap_or(current.max_agents)
        .clamp(1, fleet_orchestrator::domain::MAX_PROJECT_AGENTS);

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

    require_runner_worker_llm(llm)?;
    let mut plan = build_chat_plan(workspace_id, &args.title, &spec, profile);
    stamp_plan_llm(&mut plan, llm);
    plan.policy.requested_by = Some(user_id);
    accept_and_drive_plan(
        state,
        workspace_id,
        Some(user_id),
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
                    .unwrap_or("generalist")
                    .to_string(),
                acceptance: fleet_orchestrator::orch::default_acceptance(
                    t.worker_profile.as_deref().unwrap_or("generalist"),
                ),
                priority: t.priority.unwrap_or(100),
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: Some(1),
            }
        })
        .collect();

    require_runner_worker_llm(llm)?;
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
    plan.policy.requested_by = Some(user_id);
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
        Some(user_id),
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
    user_id: Option<Uuid>,
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
    if let Err(e) = crate::workforce::attach_run_to_goal(
        state,
        workspace_id,
        user_id,
        label,
        label,
        run_id,
        None,
    )
    .await
    {
        tracing::warn!(%workspace_id, %run_id, error = %e, "attach goal to run failed");
    }

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

    let spawned_any = ticks.iter().any(|t| t["spawned"].as_u64().unwrap_or(0) > 0);
    let host_assignable =
        fleet_orchestrator::store::select_assignable_host(&state.orch.store, workspace_id)
            .await
            .is_some();
    let waiting_for_host = !terminal && !spawned_any && !host_assignable;
    let hint = if terminal {
        "Sandboxes are ephemeral. Integration tokens stay on the control plane."
    } else if waiting_for_host {
        "No paired host — task left ready. Pair a fleet-host to run this work."
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
    } else if waiting_for_host {
        format!(
            "No paired host — «{label}» left ready · {task_count} task(s) · run {}",
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

    #[test]
    fn stamp_plan_llm_writes_provider_and_model() {
        let mut plan = build_chat_plan(Uuid::nil(), "T", "S", "coding");
        let runner = Uuid::now_v7();
        stamp_plan_llm(
            &mut plan,
            &ChatLlmContext {
                provider: "Ollama".into(),
                model: "tiny".into(),
                runner_id: Some(runner),
                effort: None,
            },
        );
        assert_eq!(plan.policy.llm_provider.as_deref(), Some("ollama"));
        assert_eq!(plan.policy.llm_model.as_deref(), Some("tiny"));
        assert_eq!(plan.policy.preferred_host_id, Some(runner));
    }

    #[test]
    fn wants_fleet_launch_detects_agent_request_not_pong() {
        assert!(wants_fleet_launch(
            "Launch one agent to write a one-line hello in work/hello.txt and finish. Use launch_agent_task."
        ));
        assert!(wants_fleet_launch("please launch_agent_plan for this"));
        assert!(wants_fleet_launch("spin up an agent to fix the tests"));
        assert!(wants_fleet_launch("start a worker on the paired runner"));
        assert!(!wants_fleet_launch("Say only: pong"));
        assert!(!wants_fleet_launch("pong"));
        assert!(!wants_fleet_launch("what is an agent?"));
        assert!(!wants_fleet_launch("launch the website tomorrow"));
        assert!(!wants_fleet_launch("don't launch an agent"));
        assert!(!wants_fleet_launch("do not spawn a worker"));
        assert!(!wants_fleet_launch("never launch a fleet run"));
    }

    #[test]
    fn runner_worker_llm_refuses_openai_and_missing_runner() {
        assert!(require_runner_worker_llm(&ChatLlmContext::default()).is_ok());
        assert!(
            require_runner_worker_llm(&ChatLlmContext {
                provider: "openai".into(),
                model: "any".into(),
                runner_id: None,
                effort: None,
            })
            .is_ok(),
            "OpenAI is a runner CLI; missing runner is require_taking_runner"
        );
        assert!(
            require_taking_runner(&ChatLlmContext {
                provider: "openai".into(),
                model: "any".into(),
                runner_id: None,
                effort: None,
            })
            .is_err()
        );
        assert!(
            require_taking_runner(&ChatLlmContext {
                provider: "gemini".into(),
                model: "any".into(),
                runner_id: None,
                effort: None,
            })
            .is_err()
        );
        assert!(
            require_runner_worker_llm(&ChatLlmContext {
                provider: "ollama".into(),
                model: "tiny".into(),
                runner_id: None,
                effort: None,
            })
            .is_ok(),
            "chat may queue ollama work until a runner appears"
        );
        assert!(
            require_taking_runner(&ChatLlmContext {
                provider: "ollama".into(),
                model: "tiny".into(),
                runner_id: None,
                effort: None,
            })
            .is_err()
        );
        assert!(
            require_taking_runner(&ChatLlmContext {
                provider: "ollama".into(),
                model: "tiny".into(),
                runner_id: Some(Uuid::now_v7()),
                effort: None,
            })
            .is_ok()
        );
    }

    #[test]
    fn parse_launch_plan_args_rejects_empty_tasks() {
        assert!(parse_launch_plan_args(&json!({ "tasks": [] })).is_err());
    }

    #[test]
    fn parse_launch_plan_args_rejects_blank_id() {
        assert!(parse_launch_plan_args(&json!({
            "tasks": [{ "id": " ", "title": "X" }]
        }))
        .is_err());
    }

    #[test]
    fn parse_launch_plan_args_accepts_two_task_dag_and_outputs() {
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
        assert_eq!(ok.tasks[0].id, "a");
        assert_eq!(ok.tasks[1].id, "b");
        assert_eq!(ok.tasks[1].depends_on, Some(vec!["a".into()]));
        assert_eq!(ok.tasks[0].worker_profile.as_deref(), Some("frontend"));
        let outs = ok.outputs.unwrap();
        assert_eq!(outs[0].provider, "notion");
        assert_eq!(outs[0].operation, "append_blocks");
    }

    #[test]
    fn parse_launch_plan_args_rejects_blank_task_title() {
        assert!(parse_launch_plan_args(&json!({
            "tasks": [{ "id": "a", "title": "  " }]
        }))
        .is_err());
    }

    #[test]
    fn parse_launch_plan_args_rejects_task_title_over_200() {
        let title = "x".repeat(201);
        assert!(parse_launch_plan_args(&json!({
            "tasks": [{ "id": "a", "title": title }]
        }))
        .is_err());
    }

    #[test]
    fn parse_launch_plan_args_rejects_over_max_tasks() {
        let tasks: Vec<_> = (0..=MAX_CHAT_PLAN_TASKS)
            .map(|i| json!({ "id": format!("t{i}"), "title": "T" }))
            .collect();
        assert_eq!(tasks.len(), 1001);
        let e = parse_launch_plan_args(&json!({ "tasks": tasks })).unwrap_err();
        assert!(e.contains("1000"), "{e}");
    }

    #[test]
    fn build_chat_plan_sets_chat_source_and_single_task() {
        let plan = build_chat_plan(
            Uuid::nil(),
            "Fix flaky tests",
            "Investigate CI flake",
            "generalist",
        );
        assert_eq!(plan.source, "chat");
        assert_eq!(plan.tasks.len(), 1);
        assert!(plan.tasks[0].depends_on.is_empty());
    }

    #[test]
    fn build_chat_plan_clips_summary_to_280_keeps_full_spec_md() {
        let spec = "x".repeat(300);
        let plan = build_chat_plan(Uuid::nil(), "Clip", &spec, "generalist");
        assert_eq!(plan.tasks[0].summary, "x".repeat(280));
        assert_eq!(plan.tasks[0].spec_md, spec);
    }

    #[test]
    fn parse_launch_args_rejects_empty_title() {
        let v = json!({ "title": "  " });
        assert!(parse_launch_args(&v).is_err());
    }

    #[test]
    fn parse_launch_args_keeps_title_and_spec() {
        let v = json!({ "title": "Do thing", "spec": "details" });
        let a = parse_launch_args(&v).unwrap();
        assert_eq!(a.title, "Do thing");
        assert_eq!(a.spec.as_deref(), Some("details"));
    }

    #[test]
    fn parse_launch_args_rejects_invalid_json_string() {
        let v = Value::String("not-json".into());
        assert!(parse_launch_args(&v).is_err());
    }

    #[test]
    fn parse_launch_args_omitted_spec_is_none() {
        let v = json!({ "title": "Do thing" });
        let a = parse_launch_args(&v).unwrap();
        assert_eq!(a.title, "Do thing");
        assert_eq!(a.spec, None);
    }

    #[test]
    fn parse_launch_args_rejects_missing_title() {
        assert!(parse_launch_args(&json!({})).is_err());
        assert!(parse_launch_args(&json!({ "spec": "x" })).is_err());
    }

    #[test]
    fn tool_definitions_include_launch_and_integrations() {
        // github_commit_files / github_create_pr stay integration_query ops, not first-class tools.
        const EXPECTED: &[&str] = &[
            "list_sandboxes",
            "list_tasks",
            "list_runs",
            "get_fleet_status",
            "launch_agent_task",
            "launch_agent_plan",
            "configure_fleet",
            "list_agent_personas",
            "read_team_chat",
            "send_team_message",
            "list_integrations",
            "get_integration",
            "integration_query",
            "start_case",
            "list_cases",
            "get_case",
            "observe_case",
            "cancel_case",
            "monitor_tickets",
            "list_tickets",
            "finish_ticket",
        ];
        assert!(!EXPECTED.contains(&"github_commit_files"));
        assert!(!EXPECTED.contains(&"github_create_pr"));
        let tools = tool_definitions();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(names, EXPECTED);
    }

    #[test]
    fn tool_definitions_launch_and_integration_query_required_fields() {
        fn required_of<'a>(tools: &'a Value, name: &str) -> Vec<&'a str> {
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["function"]["name"] == name)
                .unwrap()["function"]["parameters"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect()
        }
        let tools = tool_definitions();
        assert_eq!(required_of(&tools, "launch_agent_task"), ["title"]);
        assert_eq!(required_of(&tools, "launch_agent_plan"), ["tasks"]);
        assert_eq!(
            required_of(&tools, "integration_query"),
            ["provider", "operation"]
        );
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

    fn listed_channels_general_then_eng() -> Vec<Value> {
        vec![json!({
            "ok": true,
            "provider": "slack",
            "operation": "list_channels",
            "data": {
                "items": [
                    { "id": "CGEN", "name": "general" },
                    { "id": "CENG", "name": "eng" }
                ]
            }
        })]
    }

    #[test]
    fn coerce_resolves_hash_eng_from_listed_channels() {
        let prior = listed_channels_general_then_eng();
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
    fn coerce_leaves_real_c_channel_id_unchanged() {
        let prior = listed_channels_general_then_eng();
        assert_eq!(
            coerce_integration_args(
                "slack",
                "get_channel_history",
                &json!({ "channel": "C01234567", "limit": 10 }),
                &prior,
            ),
            None
        );
    }

    #[test]
    fn coerce_post_message_and_get_thread_resolve_hash_eng() {
        let prior = listed_channels_general_then_eng();
        for operation in ["post_message", "get_thread"] {
            let fixed = coerce_integration_args(
                "slack",
                operation,
                &json!({ "channel": "#eng" }),
                &prior,
            )
            .unwrap_or_else(|| panic!("{operation} should rewrite #eng"));
            assert_eq!(fixed["channel"], "CENG", "{operation}");
        }
    }

    #[test]
    fn retry_hint_names_required_text() {
        let h = retry_hint("notion", "append_blocks");
        assert_eq!(h["required"], json!(["page_id", "text"]));
        let example = h["example"].as_object().unwrap();
        assert!(example.contains_key("page_id"));
        assert!(example.contains_key("text"));
    }

    #[test]
    fn retry_hint_append_blocks_example_has_page_id_and_text() {
        let h = retry_hint("notion", "append_blocks");
        let page_id = h["example"]["page_id"].as_str().unwrap_or("");
        let text = h["example"]["text"].as_str().unwrap_or("");
        assert!(!page_id.is_empty(), "example.page_id");
        assert!(!text.is_empty(), "example.text");
    }

    #[test]
    fn retry_hint_unknown_pair_empty_required() {
        let h = retry_hint("nope", "nope");
        assert_eq!(h["required"], json!([]));
        assert_eq!(h["should_retry"], true);
    }

    #[test]
    fn retry_hint_slack_post_message_requires_channel_and_text() {
        let h = retry_hint("slack", "post_message");
        assert_eq!(h["required"], json!(["channel", "text"]));
    }

    fn launch_action(tool: &str, ok: bool) -> ChatAction {
        ChatAction {
            tool: tool.into(),
            ok,
            summary: "ok".into(),
            run_id: Some("r".into()),
            sandbox_ids: None,
            detail: None,
        }
    }

    #[test]
    fn continue_rewrite_does_not_inject_ship_playbook() {
        let rewritten = rewrite_if_asked_to_continue(
            "Say continue and I'll commit_files.",
            &[launch_action("launch_agent_plan", true)],
        )
        .expect("rewrite");
        assert_eq!(
            rewritten,
            "Fleet `r` is live on this screen. Stay here — task status and agent activity show in the live panel. \
I will not ask you to continue."
        );
    }

    #[test]
    fn continue_rewrite_none_without_ok_launch() {
        let ask = "Say continue";
        assert_eq!(rewrite_if_asked_to_continue(ask, &[]), None);
        assert_eq!(
            rewrite_if_asked_to_continue(ask, &[launch_action("launch_agent_plan", false)]),
            None
        );
        assert_eq!(
            rewrite_if_asked_to_continue(ask, &[launch_action("list_runs", true)]),
            None
        );
    }

    #[test]
    fn continue_rewrite_none_when_reply_does_not_ask() {
        assert_eq!(
            rewrite_if_asked_to_continue(
                "Fleet launched, watching the panel.",
                &[launch_action("launch_agent_plan", true)],
            ),
            None
        );
    }

    #[test]
    fn connector_catalog_lists_every_provider_and_ops() {
        let md = connector_catalog_markdown(&["slack".into()]);
        assert!(md.contains("slack [connected]"), "{md}");
        assert!(md.contains("github [not connected]"), "{md}");
    }

    #[test]
    fn connector_catalog_empty_connected_includes_integrations_hint() {
        let md = connector_catalog_markdown(&[]);
        assert!(
            md.contains("None connected yet. User can connect them under Integrations."),
            "{md}"
        );
        assert!(md.contains(" [not connected]:"), "{md}");
        assert!(!md.contains(" [connected]:"), "{md}");
    }

    #[test]
    fn connector_catalog_github_line_lists_commit_files_and_create_pr() {
        let md = connector_catalog_markdown(&["slack".into()]);
        let line = md
            .lines()
            .find(|l| l.starts_with("- github ["))
            .unwrap_or("");
        assert!(line.contains("commit_files"), "{line}\n{md}");
        assert!(line.contains("create_pr"), "{line}\n{md}");
    }

    #[test]
    fn connector_catalog_unknown_connected_id_does_not_invent_a_row() {
        let md = connector_catalog_markdown(&["notaprovider".into()]);
        assert!(!md.contains("- notaprovider ["), "{md}");
    }
}
