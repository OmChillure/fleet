//! Inbox ingest: the Inbox page (Start monitor) or chat tool `monitor_tickets`
//! watches connected sources. Chat `finish_ticket` loads the spec for the model
//! to plan. Inbox Finish launches one generalist — the worker chooses tools.
//!
//! Sources: GitHub / GitLab issues, Jira, Linear, Slack, Notion.
//! Tokens stay on the control plane.

use std::time::Duration;

use chrono::Utc;
use fleet_auth::{AppError, AppResult};
use fleet_db::repos::integrations as integ_repo;
use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox, UpsertMonitor};
use fleet_db::TicketInboxItem;
use fleet_integrations::proxy::{self, ProxyCaller, ProxyRequest, ProxyResult};
use fleet_orchestrator::domain::{ExternalRef, PlanTask};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

pub const POLL_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// Every integration that can feed the inbox. Poll uses whatever is connected.
pub const TASK_PROVIDERS: &[&str] = &[
    "github",
    "gitlab",
    "bitbucket",
    "jira",
    "linear",
    "trello",
    "asana",
    "slack",
    "teams",
    "notion",
    "confluence",
];

#[derive(Debug, Clone, Deserialize)]
pub struct MonitorArgs {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub providers: Option<Vec<String>>,
    #[serde(default)]
    pub github_repos: Option<Vec<String>>,
    #[serde(default)]
    pub jira_jql: Option<String>,
    #[serde(default)]
    pub slack_channels: Option<Vec<String>>,
    #[serde(default)]
    pub notion_databases: Option<Vec<String>>,
    #[serde(default)]
    pub auto_launch: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FinishTicketArgs {
    /// Free-form: `ENG-123`, `PROJ-1`, `owner/repo#12`, or `github:owner/repo#12`.
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParsedTicket {
    pub provider: Option<String>,
    pub key: String,
}

pub fn parse_ticket_ref(raw: &str) -> Option<ParsedTicket> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some((prefix, rest)) = s.split_once(':') {
        let provider = prefix.trim().to_ascii_lowercase();
        if TASK_PROVIDERS.contains(&provider.as_str()) {
            let key = rest.trim();
            if key.is_empty() {
                return None;
            }
            return Some(ParsedTicket {
                provider: Some(provider),
                key: normalize_github_key(key),
            });
        }
    }
    if let Some(key) = github_key_from_url_or_slash(s) {
        return Some(ParsedTicket {
            provider: Some("github".into()),
            key,
        });
    }
    Some(ParsedTicket {
        provider: None,
        key: s.to_string(),
    })
}

fn normalize_github_key(raw: &str) -> String {
    github_key_from_url_or_slash(raw).unwrap_or_else(|| raw.trim().to_string())
}

fn github_key_from_url_or_slash(raw: &str) -> Option<String> {
    let s = raw.trim();
    if let Some(rest) = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
    {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 4 && (parts[2] == "issues" || parts[2] == "pull") {
            let num = parts[3].split(|c: char| !c.is_ascii_digit()).next()?;
            if !num.is_empty() {
                return Some(format!("{}/{}#{num}", parts[0], parts[1]));
            }
        }
    }
    if let Some((repo, num)) = s.split_once('#') {
        let repo = repo.trim();
        let num = num
            .trim()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        if repo.contains('/') && !num.is_empty() {
            return Some(format!("{repo}#{num}"));
        }
    }
    None
}

pub fn parse_github_owner_repo_num(key: &str) -> Option<(String, String, u64)> {
    let key = normalize_github_key(key);
    let (repo, num) = key.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let n = num.parse().ok()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string(), n))
}

/// Finish, HTTP monitor, and chat `monitor_tickets` share this gate.
/// Unknown names are 400 — never silently dropped (that would still
/// upsert `auto_launch` or insert a run).
fn require_task_provider(raw: &str) -> AppResult<String> {
    let p = raw.trim().to_ascii_lowercase();
    if TASK_PROVIDERS.iter().any(|known| *known == p) {
        Ok(p)
    } else {
        Err(AppError::bad_request(format!(
            "unknown inbox provider `{p}`"
        )))
    }
}

/// Explicit monitor provider list. Any name not in TASK_PROVIDERS is 400.
/// Blank entries are ignored. Empty result is Ok — caller may fall back
/// to connected sources.
fn require_requested_task_providers(raw: &[String]) -> AppResult<Vec<String>> {
    let mut out = Vec::new();
    for p in raw {
        if p.trim().is_empty() {
            continue;
        }
        out.push(require_task_provider(p)?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Resolve the monitor listen list. Unknown names 400 (never silently
/// dropped). An empty request falls back to connected sources. Enabling
/// with nothing connected is 400 — never upsert `auto_launch` on a blank
/// listen list (UI Start monitor / chat `monitor_tickets`).
fn resolve_monitor_providers(
    requested: &[String],
    connected: &[String],
    enabled: bool,
) -> AppResult<Vec<String>> {
    let mut providers = require_requested_task_providers(requested)?;
    if providers.is_empty() {
        providers = connected.to_vec();
    }
    if enabled && providers.is_empty() {
        return Err(AppError::bad_request(
            "no listen source connected — open Integrations, Connect an app, then Start monitor on Inbox",
        ));
    }
    providers.sort();
    providers.dedup();
    Ok(providers)
}

/// `auto_launch` is only persisted when there is something to listen to.
/// Disabled empty + `auto_launch: true` must not arm a later Start monitor.
fn persist_monitor_auto_launch(providers: &[String], requested: Option<bool>) -> bool {
    !providers.is_empty() && requested.unwrap_or(false)
}

pub async fn monitor_tickets(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: MonitorArgs,
) -> AppResult<Value> {
    let enabled = args.enabled.unwrap_or(true);
    let connected = connected_task_providers(state, workspace_id).await?;
    let providers =
        resolve_monitor_providers(&args.providers.unwrap_or_default(), &connected, enabled)?;

    let github_repos: Vec<String> = args
        .github_repos
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.trim().to_string())
        .filter(|r| r.contains('/'))
        .collect();
    let jira_jql = args
        .jira_jql
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let slack_channels: Vec<String> = args
        .slack_channels
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let notion_databases: Vec<String> = args
        .notion_databases
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let auto_launch = persist_monitor_auto_launch(&providers, args.auto_launch);

    let monitor = ticket_repo::upsert_monitor(
        &state.db,
        &UpsertMonitor {
            workspace_id,
            enabled,
            providers: providers.clone(),
            github_repos,
            jira_jql,
            slack_channels,
            notion_databases,
            auto_launch,
            started_by: Some(user_id),
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    if enabled {
        let _ = state
            .orch
            .store
            .set_auto_sync_external(workspace_id, true)
            .await;
    }

    let poll = if enabled {
        Some(poll_workspace(state, workspace_id, true).await?)
    } else {
        None
    };
    let tickets = ticket_repo::list_inbox(&state.db, workspace_id, None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let content = json!({
        "enabled": monitor.enabled,
        "providers": monitor.providers,
        "github_repos": monitor.github_repos,
        "jira_jql": monitor.jira_jql,
        "slack_channels": monitor.slack_channels,
        "notion_databases": monitor.notion_databases,
        "auto_launch": monitor.auto_launch,
        "last_poll_at": monitor.last_poll_at,
        "last_error": monitor.last_error,
        "poll": poll,
        "tickets": tickets_json(&tickets),
    });
    Ok(content)
}

pub async fn list_tickets(
    state: &AppState,
    workspace_id: Uuid,
    state_filter: Option<&str>,
) -> AppResult<Value> {
    let monitor = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let tickets = ticket_repo::list_inbox(&state.db, workspace_id, state_filter)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let content = json!({
        "monitor": monitor.as_ref().map(|m| json!({
            "enabled": m.enabled,
            "providers": m.providers,
            "github_repos": m.github_repos,
            "jira_jql": m.jira_jql,
            "slack_channels": m.slack_channels,
            "notion_databases": m.notion_databases,
            "auto_launch": m.auto_launch,
            "last_poll_at": m.last_poll_at,
            "last_error": m.last_error,
        })),
        "tickets": tickets_json(&tickets),
        "count": tickets.len(),
    });
    Ok(content)
}

pub struct PreparedFinish {
    pub item: TicketInboxItem,
    pub tasks: Vec<PlanTask>,
    pub title: String,
}

/// Inbox finish/monitor are scoped to the route workspace. Missing membership
/// is 403 — same error the HTTP handlers return from `require_member`.
fn require_workspace_role(role: Option<String>) -> AppResult<String> {
    role.ok_or_else(|| AppError::forbidden("not a member of this workspace"))
}

/// Knowing a ticket id from workspace A must not finish or monitor it via B.
/// Wrong workspace is 403 (or 404); never launch or mutate that row.
fn require_ticket_workspace(item: &TicketInboxItem, workspace_id: Uuid) -> AppResult<()> {
    if item.workspace_id != workspace_id {
        return Err(AppError::forbidden("ticket is not in this workspace"));
    }
    Ok(())
}

pub async fn prepare_finish(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: FinishTicketArgs,
) -> AppResult<PreparedFinish> {
    let item = resolve_ticket(state, workspace_id, &args).await?;
    require_ticket_workspace(&item, workspace_id)?;
    let live = fetch_ticket_details(state, workspace_id, user_id, &item).await;
    let spec = format!(
        "{}\n\n# I/O\n\
         Source: `{provider}` `{key}`.\n\
         Use `use_integration` for whatever this item actually needs on any connected app. \
         Tokens stay on the control plane. Do not assume you must write code or open a PR.\n\
         When you finish, write `result.md`. The control plane will also reply on this source.",
        ticket_spec_md(&item, live.as_ref()),
        provider = item.provider,
        key = item.external_key,
    );
    let title = format!("{}: {}", item.external_key, item.title);
    let ext = ExternalRef {
        provider: item.provider.clone(),
        key: item.external_key.clone(),
    };
    let tasks = vec![PlanTask {
        plan_task_id: "work".into(),
        title: title.clone(),
        summary: spec.chars().take(280).collect(),
        spec_md: spec,
        depends_on: vec![],
        worker_profile: "generalist".into(),
        acceptance: vec!["result.md exists and is non-empty".into()],
        priority: 100,
        external_ref: Some(ext),
        timeout_s: Some(3600),
        max_retries: Some(1),
    }];
    Ok(PreparedFinish { item, tasks, title })
}

/// Chat tool: return the ticket spec so the model plans. Does not launch a run.
pub async fn ticket_context_for_model(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: FinishTicketArgs,
) -> AppResult<Value> {
    let prep = prepare_finish(state, workspace_id, user_id, args).await?;
    let spec = prep
        .tasks
        .first()
        .map(|t| t.spec_md.clone())
        .unwrap_or_default();
    Ok(json!({
        "ok": true,
        "launched": false,
        "title": prep.title,
        "ticket": {
            "provider": prep.item.provider,
            "key": prep.item.external_key,
            "title": prep.item.title,
            "state": prep.item.state,
        },
        "spec": spec,
        "hint": "You plan the next step. Use integration_query for one-shot I/O, or launch_agent_plan with the tasks and outputs this item needs. Do not assume coding, review, or a PR.",
    }))
}

pub async fn mark_ticket_launched(
    state: &AppState,
    workspace_id: Uuid,
    provider: &str,
    key: &str,
    run_id: Uuid,
) -> AppResult<()> {
    ticket_repo::mark_launched(&state.db, workspace_id, provider, key, run_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(())
}

/// Second Finish of the same inbox key reuses this run instead of inserting another.
fn already_launched_run(item: &TicketInboxItem) -> Option<Uuid> {
    if item.state.eq_ignore_ascii_case("launched") || item.run_id.is_some() {
        item.run_id
    } else {
        None
    }
}

pub async fn poll_workspace(state: &AppState, workspace_id: Uuid, force: bool) -> AppResult<Value> {
    let Some(monitor) = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    else {
        return Ok(json!({ "skipped": true, "reason": "no monitor" }));
    };
    if !monitor.enabled {
        return Ok(json!({ "skipped": true, "reason": "disabled" }));
    }
    if !force {
        if let Some(last) = monitor.last_poll_at {
            if Utc::now().signed_duration_since(last)
                < chrono::Duration::from_std(POLL_MIN_INTERVAL).unwrap_or_default()
            {
                return Ok(json!({ "skipped": true, "reason": "throttled" }));
            }
        }
    }

    let caller = ProxyCaller::System;
    let mut errors: Vec<String> = Vec::new();
    let mut seen = 0usize;
    for provider in &monitor.providers {
        match provider.as_str() {
            "github" => {
                match poll_github(state, workspace_id, &monitor.github_repos, &caller).await {
                    Ok(n) => seen += n,
                    Err(e) => errors.push(format!("github: {e}")),
                }
            }
            "jira" => {
                match poll_jira(state, workspace_id, monitor.jira_jql.as_deref(), &caller).await {
                    Ok(n) => seen += n,
                    Err(e) => errors.push(format!("jira: {e}")),
                }
            }
            "linear" => match poll_linear(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("linear: {e}")),
            },
            "gitlab" => match poll_gitlab(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("gitlab: {e}")),
            },
            "slack" => {
                match poll_slack(state, workspace_id, &monitor.slack_channels, &caller).await {
                    Ok(n) => seen += n,
                    Err(e) => errors.push(format!("slack: {e}")),
                }
            }
            "notion" => {
                match poll_notion(state, workspace_id, &monitor.notion_databases, &caller).await {
                    Ok(n) => seen += n,
                    Err(e) => errors.push(format!("notion: {e}")),
                }
            }
            "trello" => match poll_trello(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("trello: {e}")),
            },
            "asana" => match poll_asana(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("asana: {e}")),
            },
            "teams" => match poll_teams(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("teams: {e}")),
            },
            "bitbucket" => match poll_bitbucket(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("bitbucket: {e}")),
            },
            "confluence" => match poll_confluence(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("confluence: {e}")),
            },
            other => errors.push(format!("{other}: no inbox adapter")),
        }
    }
    let err_join = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };
    ticket_repo::mark_polled(&state.db, workspace_id, Utc::now(), err_join.as_deref())
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(json!({
        "seen": seen,
        "errors": errors,
    }))
}

/// New Slack/Notion (etc.) inbox rows that should start a fleet without a human click.
pub async fn list_auto_launch_items(
    state: &AppState,
    workspace_id: Uuid,
) -> AppResult<Vec<TicketInboxItem>> {
    let Some(monitor) = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    else {
        return Ok(Vec::new());
    };
    if !monitor.enabled || !monitor.auto_launch {
        return Ok(Vec::new());
    }
    let open = ticket_repo::list_inbox(&state.db, workspace_id, Some("open"))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let cutoff = Utc::now() - chrono::Duration::minutes(5);
    Ok(open
        .into_iter()
        .filter(|i| i.run_id.is_none() && i.created_at >= cutoff)
        .take(8)
        .collect())
}

pub async fn poll_due_workspaces(state: &AppState) {
    let ids = match ticket_repo::list_enabled_workspace_ids(&state.db).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "ticket monitor: list enabled failed");
            return;
        }
    };
    for id in ids {
        if let Err(e) = poll_workspace(state, id, false).await {
            tracing::warn!(workspace_id = %id, error = %e, "ticket monitor poll failed");
        }
    }
}

async fn connected_task_providers(state: &AppState, workspace_id: Uuid) -> AppResult<Vec<String>> {
    let rows = integ_repo::list_for_workspace(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|c| c.provider.to_ascii_lowercase())
        .filter(|p| TASK_PROVIDERS.contains(&p.as_str()))
        .collect())
}

async fn proxy_op(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
    provider: &str,
    operation: &str,
    args: Value,
) -> ProxyResult {
    proxy::invoke(
        &state.db,
        &state.integrations.credentials_key,
        ProxyRequest {
            workspace_id,
            provider: provider.into(),
            operation: operation.into(),
            args,
            caller: caller.clone(),
        },
    )
    .await
}

async fn poll_github(
    state: &AppState,
    workspace_id: Uuid,
    repos: &[String],
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let mut targets = repos.to_vec();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "github",
            "list_repos",
            json!({ "per_page": 5 }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        if let Some(arr) = listed.data.get("items").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(name) = r.get("full_name").and_then(|v| v.as_str()) {
                    targets.push(name.to_string());
                }
            }
        }
    }
    let mut n = 0usize;
    for repo in targets.iter().take(8) {
        let Some((owner, name)) = repo.split_once('/') else {
            continue;
        };
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "github",
            "list_issues",
            json!({ "owner": owner, "repo": name, "state": "open", "per_page": 20 }),
        )
        .await;
        if !listed.ok {
            return Err(format!("{repo}: {}", listed.summary));
        }
        n += upsert_github_items(state, workspace_id, owner, name, &listed.data).await?;
    }
    Ok(n)
}

async fn upsert_github_items(
    state: &AppState,
    workspace_id: Uuid,
    owner: &str,
    repo: &str,
    data: &Value,
) -> Result<usize, String> {
    let items = data.get("items").and_then(|v| v.as_array());
    let Some(items) = items else {
        return Ok(0);
    };
    let mut n = 0usize;
    for i in items {
        let number = i
            .get("number")
            .and_then(|v| v.as_u64())
            .or_else(|| i.get("number").and_then(|v| v.as_i64()).map(|n| n as u64));
        let Some(number) = number else {
            continue;
        };
        let title = i
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled")
            .to_string();
        let url = i
            .get("html_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let body = i.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let closed = i.get("state").and_then(|v| v.as_str()) == Some("closed");
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: "github".into(),
                external_key: format!("{owner}/{repo}#{number}"),
                title,
                url,
                summary: body.chars().take(4000).collect(),
                provider_closed: closed,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

async fn poll_jira(
    state: &AppState,
    workspace_id: Uuid,
    jql: Option<&str>,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let jql = jql
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("statusCategory != Done ORDER BY updated DESC");
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "jira",
        "search_issues",
        json!({ "jql": jql, "max_results": 20 }),
    )
    .await;
    if !listed.ok {
        return Err(listed.summary);
    }
    let items = listed
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut n = 0usize;
    for i in items {
        let key = i
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if key.is_empty() {
            continue;
        }
        let title = i
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or(key.as_str())
            .to_string();
        let status = i.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let closed = status_done(status);
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: "jira".into(),
                external_key: key,
                title,
                url: None,
                summary: format!("Jira status: {status}"),
                provider_closed: closed,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

async fn poll_linear(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "linear",
        "list_issues",
        json!({ "limit": 20 }),
    )
    .await;
    if !listed.ok {
        return Err(listed.summary);
    }
    let items = listed
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut n = 0usize;
    for i in items {
        let key = i
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if key.is_empty() {
            continue;
        }
        let title = i
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(key.as_str())
            .to_string();
        let url = i.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
        let desc = i.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let state_type = i.get("state_type").and_then(|v| v.as_str()).unwrap_or("");
        let closed = matches!(state_type, "completed" | "canceled");
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: "linear".into(),
                external_key: key,
                title,
                url,
                summary: desc.chars().take(4000).collect(),
                provider_closed: closed,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

async fn poll_gitlab(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "gitlab",
        "list_issues",
        json!({ "state": "opened", "per_page": 20 }),
    )
    .await;
    if !listed.ok {
        return Err(listed.summary);
    }
    let items = listed
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut n = 0usize;
    for i in items {
        let iid = i
            .get("iid")
            .or_else(|| i.get("number"))
            .and_then(|v| v.as_u64())
            .or_else(|| i.get("iid").and_then(|v| v.as_i64()).map(|n| n as u64));
        let path = i
            .get("project")
            .or_else(|| i.get("project_path"))
            .or_else(|| i.get("references"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(iid) = iid else {
            continue;
        };
        let key = if path.contains('/') {
            format!("{path}#{iid}")
        } else if let Some(p) = i.get("web_url").and_then(|v| v.as_str()) {
            p.to_string()
        } else {
            format!("#{iid}")
        };
        let title = i
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled")
            .to_string();
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: "gitlab".into(),
                external_key: key,
                title,
                url: i.get("web_url").and_then(|v| v.as_str()).map(|s| s.into()),
                summary: i
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(4000)
                    .collect(),
                provider_closed: i.get("state").and_then(|v| v.as_str()) == Some("closed"),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

async fn poll_slack(
    state: &AppState,
    workspace_id: Uuid,
    channels: &[String],
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let mut targets = channels.to_vec();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "slack",
            "list_channels",
            json!({ "limit": 15 }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        if let Some(arr) = listed.data.get("items").and_then(|v| v.as_array()) {
            for c in arr.iter().take(8) {
                if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
                    targets.push(id.to_string());
                }
            }
        }
    }
    let mut n = 0usize;
    for channel in targets.iter().take(8) {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "slack",
            "get_channel_history",
            json!({ "channel": channel, "limit": 15 }),
        )
        .await;
        if !listed.ok {
            return Err(format!("{channel}: {}", listed.summary));
        }
        let items = listed
            .data
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for m in items {
            let ts = m.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            if ts.is_empty() {
                continue;
            }
            let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            if text.starts_with("Fleet run ") {
                continue;
            }
            let title: String = text.chars().take(80).collect();
            ticket_repo::upsert_inbox(
                &state.db,
                &UpsertInbox {
                    workspace_id,
                    provider: "slack".into(),
                    external_key: format!("{channel}:{ts}"),
                    title,
                    url: None,
                    summary: text.chars().take(4000).collect(),
                    provider_closed: false,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    Ok(n)
}

async fn poll_notion(
    state: &AppState,
    workspace_id: Uuid,
    databases: &[String],
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let mut n = 0usize;
    if databases.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "notion",
            "search",
            json!({ "page_size": 15, "filter": { "value": "page", "property": "object" } }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        n += upsert_notion_items(state, workspace_id, &listed.data).await?;
    } else {
        for db in databases.iter().take(6) {
            let listed = proxy_op(
                state,
                workspace_id,
                caller,
                "notion",
                "query_database",
                json!({ "database_id": db, "page_size": 15 }),
            )
            .await;
            if !listed.ok {
                return Err(format!("{db}: {}", listed.summary));
            }
            n += upsert_notion_items(state, workspace_id, &listed.data).await?;
        }
    }
    Ok(n)
}

async fn upsert_notion_items(
    state: &AppState,
    workspace_id: Uuid,
    data: &Value,
) -> Result<usize, String> {
    let items = data
        .get("items")
        .or_else(|| data.get("results"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut n = 0usize;
    for i in items {
        let id = i
            .get("id")
            .or_else(|| i.get("page_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let title = i
            .get("title")
            .and_then(|v| v.as_str())
            .or_else(|| i.get("name").and_then(|v| v.as_str()))
            .unwrap_or("Notion page")
            .to_string();
        let url = i.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: "notion".into(),
                external_key: id.to_string(),
                title,
                url,
                summary: i
                    .get("plain_text")
                    .or_else(|| i.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(4000)
                    .collect(),
                provider_closed: false,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

async fn upsert_inbox_item(
    state: &AppState,
    workspace_id: Uuid,
    provider: &str,
    key: String,
    title: String,
    url: Option<String>,
    summary: String,
    closed: bool,
) -> Result<(), String> {
    if key.trim().is_empty() {
        return Ok(());
    }
    ticket_repo::upsert_inbox(
        &state.db,
        &UpsertInbox {
            workspace_id,
            provider: provider.into(),
            external_key: key,
            title,
            url,
            summary,
            provider_closed: closed,
        },
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn json_items(data: &Value) -> Vec<Value> {
    data.get("items")
        .or_else(|| data.get("results"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn json_text(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v
            .get(*k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return s.to_string();
        }
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return n.to_string();
        }
        if let Some(n) = v.get(*k).and_then(|x| x.as_i64()) {
            return n.to_string();
        }
    }
    String::new()
}

async fn poll_trello(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let boards = proxy_op(
        state,
        workspace_id,
        caller,
        "trello",
        "list_boards",
        json!({}),
    )
    .await;
    if !boards.ok {
        return Err(boards.summary);
    }
    let mut n = 0usize;
    for b in json_items(&boards.data).into_iter().take(6) {
        let board_id = json_text(&b, &["id", "idBoard"]);
        if board_id.is_empty() {
            continue;
        }
        let cards = proxy_op(
            state,
            workspace_id,
            caller,
            "trello",
            "list_cards",
            json!({ "board_id": board_id }),
        )
        .await;
        if !cards.ok {
            return Err(cards.summary);
        }
        for c in json_items(&cards.data) {
            let key = json_text(&c, &["id"]);
            let title = json_text(&c, &["name", "title"]);
            let closed = c.get("closed").and_then(|v| v.as_bool()).unwrap_or(false);
            upsert_inbox_item(
                state,
                workspace_id,
                "trello",
                key,
                title,
                c.get("url").and_then(|v| v.as_str()).map(|s| s.into()),
                json_text(&c, &["desc"]),
                closed,
            )
            .await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn poll_asana(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let spaces = proxy_op(
        state,
        workspace_id,
        caller,
        "asana",
        "list_workspaces",
        json!({}),
    )
    .await;
    if !spaces.ok {
        return Err(spaces.summary);
    }
    let ws_gid = json_items(&spaces.data).into_iter().find_map(|w| {
        let id = json_text(&w, &["gid", "id"]);
        (!id.is_empty()).then_some(id)
    });
    let Some(ws_gid) = ws_gid else {
        return Ok(0);
    };
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "asana",
        "list_tasks",
        json!({ "workspace": ws_gid }),
    )
    .await;
    if !listed.ok {
        return Err(listed.summary);
    }
    let mut n = 0usize;
    for t in json_items(&listed.data) {
        let key = json_text(&t, &["gid", "id"]);
        let title = json_text(&t, &["name", "title"]);
        let closed = t
            .get("completed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        upsert_inbox_item(
            state,
            workspace_id,
            "asana",
            key,
            title,
            t.get("permalink_url")
                .and_then(|v| v.as_str())
                .map(|s| s.into()),
            json_text(&t, &["notes"]),
            closed,
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

async fn poll_teams(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let chats = proxy_op(
        state,
        workspace_id,
        caller,
        "teams",
        "list_chats",
        json!({ "limit": 8 }),
    )
    .await;
    if !chats.ok {
        return Err(chats.summary);
    }
    let mut n = 0usize;
    for chat in json_items(&chats.data).into_iter().take(6) {
        let chat_id = json_text(&chat, &["id"]);
        if chat_id.is_empty() {
            continue;
        }
        let msgs = proxy_op(
            state,
            workspace_id,
            caller,
            "teams",
            "list_chat_messages",
            json!({ "chat_id": chat_id, "limit": 15 }),
        )
        .await;
        if !msgs.ok {
            return Err(msgs.summary);
        }
        for m in json_items(&msgs.data) {
            let mid = json_text(&m, &["id"]);
            let text = {
                let top = json_text(&m, &["text", "content"]);
                if !top.is_empty() {
                    top
                } else {
                    m.get("body")
                        .and_then(|b| b.get("content").or(b.as_str().map(|_| b)))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string()
                }
            };
            if mid.is_empty() || text.is_empty() || text.starts_with("Fleet run ") {
                continue;
            }
            let title: String = text.chars().take(80).collect();
            upsert_inbox_item(
                state,
                workspace_id,
                "teams",
                format!("{chat_id}:{mid}"),
                title,
                None,
                text.chars().take(4000).collect(),
                false,
            )
            .await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn poll_bitbucket(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let repos = proxy_op(
        state,
        workspace_id,
        caller,
        "bitbucket",
        "list_repos",
        json!({ "per_page": 5 }),
    )
    .await;
    if !repos.ok {
        return Err(repos.summary);
    }
    let mut n = 0usize;
    for r in json_items(&repos.data).into_iter().take(5) {
        let full = json_text(&r, &["full_name"]);
        let Some((ws, repo)) = full.split_once('/') else {
            continue;
        };
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "bitbucket",
            "list_issues",
            json!({ "workspace": ws, "repo": repo }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        for i in json_items(&listed.data) {
            let id = json_text(&i, &["id"]);
            if id.is_empty() {
                continue;
            }
            let key = format!("{full}#{id}");
            let title = json_text(&i, &["title", "name"]);
            let issue_state = json_text(&i, &["state"]);
            upsert_inbox_item(
                state,
                workspace_id,
                "bitbucket",
                key,
                title,
                i.get("links").and_then(|v| v.as_str()).map(|s| s.into()),
                json_text(&i, &["content"]),
                status_done(&issue_state),
            )
            .await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn poll_confluence(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
) -> Result<usize, String> {
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "confluence",
        "search",
        json!({ "query": "type=page", "limit": 20 }),
    )
    .await;
    if !listed.ok {
        return Err(listed.summary);
    }
    let mut n = 0usize;
    for p in json_items(&listed.data) {
        let key = json_text(&p, &["id"]);
        let title = json_text(&p, &["title"]);
        upsert_inbox_item(
            state,
            workspace_id,
            "confluence",
            key,
            title,
            p.get("links").and_then(|v| v.as_str()).map(|s| s.into()),
            json_text(&p, &["body"]),
            json_text(&p, &["status"]) == "trashed",
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

fn status_done(status: &str) -> bool {
    let s = status.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    // Substring match would mark "Incomplete" / "Not Done" closed and hide open work.
    // Same for Jira/Asana/Linear "not completed" / "not closed" / "not resolved".
    if s.contains("incomplete")
        || s.contains("not done")
        || s.contains("unclosed")
        || s.contains("unresolved")
        || s.contains("reopened")
        || s.contains("not completed")
        || s.contains("not closed")
        || s.contains("not resolved")
    {
        return false;
    }
    s.contains("done") || s.contains("closed") || s.contains("resolved") || s.contains("complete")
}

async fn resolve_ticket(
    state: &AppState,
    workspace_id: Uuid,
    args: &FinishTicketArgs,
) -> AppResult<TicketInboxItem> {
    let raw = args
        .ticket
        .as_deref()
        .or(args.key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let parsed = raw.and_then(parse_ticket_ref);
    let provider = args
        .provider
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .or_else(|| parsed.as_ref().and_then(|p| p.provider.clone()));
    if let Some(p) = provider.as_deref() {
        require_task_provider(p)?;
    }
    let key = parsed.as_ref().map(|p| p.key.clone());

    if let (Some(provider), Some(key)) = (provider.as_deref(), key.as_deref()) {
        if let Some(hit) = ticket_repo::find_inbox(&state.db, workspace_id, provider, key)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
        {
            return Ok(hit);
        }
        // Not seen yet — seed a stub so finish still works from a pasted key.
        return ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: provider.to_string(),
                external_key: key.to_string(),
                title: key.to_string(),
                url: None,
                summary: String::new(),
                provider_closed: false,
            },
        )
        .await
        .map_err(|e| AppError::internal(e.to_string()));
    }

    if let Some(key) = key.as_deref() {
        let hits = ticket_repo::find_inbox_by_key(&state.db, workspace_id, key)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        if hits.len() == 1 {
            return Ok(hits.into_iter().next().unwrap());
        }
        if hits.len() > 1 {
            let keys: Vec<String> = hits
                .iter()
                .map(|t| format!("{}:{}", t.provider, t.external_key))
                .collect();
            return Err(AppError::bad_request(format!(
                "that key matches more than one board ({}) — pass provider too",
                keys.join(", ")
            )));
        }
    }

    let open = ticket_repo::list_inbox(&state.db, workspace_id, Some("open"))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if open.len() == 1 {
        return Ok(open.into_iter().next().unwrap());
    }
    if open.is_empty() {
        return Err(AppError::bad_request(
            "no open tickets — Start monitor on Inbox first (after connecting apps), or pass a key like ENG-123",
        ));
    }
    let list: Vec<String> = open
        .iter()
        .take(8)
        .map(|t| format!("{} {}", t.external_key, t.title))
        .collect();
    Err(AppError::bad_request(format!(
        "which ticket? inbox has {}: {}",
        open.len(),
        list.join("; ")
    )))
}

async fn fetch_ticket_details(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    item: &TicketInboxItem,
) -> Option<Value> {
    let caller = ProxyCaller::Chat { user_id };
    let (op, args) = match item.provider.as_str() {
        "github" => {
            let (owner, repo, number) = parse_github_owner_repo_num(&item.external_key)?;
            (
                "get_issue",
                json!({ "owner": owner, "repo": repo, "number": number }),
            )
        }
        "gitlab" => {
            let (project, iid) = item.external_key.split_once('#')?;
            ("get_issue", json!({ "project": project, "iid": iid }))
        }
        "bitbucket" => {
            let (full, id) = item.external_key.split_once('#')?;
            let (ws, repo) = full.split_once('/')?;
            (
                "get_issue",
                json!({ "workspace": ws, "repo": repo, "id": id }),
            )
        }
        "jira" => ("get_issue", json!({ "key": item.external_key })),
        "linear" => ("get_issue", json!({ "identifier": item.external_key })),
        "trello" => ("get_card", json!({ "card_id": item.external_key })),
        "asana" => ("get_task", json!({ "gid": item.external_key })),
        "notion" => ("get_page", json!({ "page_id": item.external_key })),
        "confluence" => ("get_page", json!({ "id": item.external_key })),
        _ => return None,
    };
    let result = proxy_op(state, workspace_id, &caller, &item.provider, op, args).await;
    if result.ok {
        Some(result.data)
    } else {
        None
    }
}

fn ticket_spec_md(item: &TicketInboxItem, live: Option<&Value>) -> String {
    let mut out = format!(
        "# Ticket `{key}` ({provider})\n\n**{title}**\n\n",
        key = item.external_key,
        provider = item.provider,
        title = item.title,
    );
    if let Some(url) = item.url.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("URL: {url}\n\n"));
    }
    if let Some(live) = live {
        if let Some(body) = live
            .get("body")
            .or_else(|| live.get("description"))
            .or_else(|| live.get("summary"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push_str(body);
            out.push_str("\n\n");
        } else {
            out.push_str(&format!(
                "```json\n{}\n```\n\n",
                serde_json::to_string_pretty(live).unwrap_or_else(|_| live.to_string())
            ));
        }
    } else if !item.summary.is_empty() {
        out.push_str(&item.summary);
        out.push_str("\n\n");
    }
    out.push_str(
        "Implement this ticket in the shared `work/` workspace. Do not invent \
         a different scope. When done, write `result.md` with what changed.\n",
    );
    out
}

pub async fn inbox_json_for_overview(state: &AppState, workspace_id: Uuid) -> Value {
    match ticket_repo::list_inbox(&state.db, workspace_id, None).await {
        Ok(items) => tickets_json(&items),
        Err(_) => json!([]),
    }
}

pub fn tickets_json(items: &[TicketInboxItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "provider": t.provider,
                    "key": t.external_key,
                    "title": t.title,
                    "url": t.url,
                    "state": t.state,
                    "run_id": t.run_id,
                    "last_seen_at": t.last_seen_at,
                })
            })
            .collect(),
    )
}

// ── HTTP ────────────────────────────────────────────────────────────────────

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{require_user, AuthState};
use fleet_db::repos::workspaces as workspace_repo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/inbox",
            get(http_list).post(http_monitor),
        )
        .route("/api/workspaces/{workspace_id}/inbox/poll", post(http_poll))
        .route(
            "/api/workspaces/{workspace_id}/inbox/finish",
            post(http_finish),
        )
}

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let auth = AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    };
    let user = require_user(&auth, jar).await?;
    let role =
        require_workspace_role(workspace_repo::user_role(&state.db, workspace_id, user.id).await?)?;
    Ok((user, role))
}

#[derive(Debug, Deserialize)]
struct InboxQuery {
    /// Free string so `GET /inbox?state=` cannot 500 on unknown/junk.
    /// Empty/whitespace is unfiltered (`inbox_state_query_filter`).
    #[serde(default)]
    state: Option<String>,
}

/// `GET /inbox?state=` filter. Empty/whitespace is unfiltered. Unknown values
/// stay `Some` so the list is an empty match, not a 500.
fn inbox_state_query_filter(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|s| !s.is_empty())
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<InboxQuery>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let filter = inbox_state_query_filter(q.state.as_deref());
    Ok(Json(list_tickets(&state, workspace_id, filter).await?))
}

async fn http_monitor(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(args): Json<MonitorArgs>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    Ok(Json(
        monitor_tickets(&state, workspace_id, user.id, args).await?,
    ))
}

async fn http_poll(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    Ok(Json(poll_workspace(&state, workspace_id, true).await?))
}

async fn http_finish(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(args): Json<FinishTicketArgs>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    Ok(Json(
        finish_and_launch(&state, workspace_id, user.id, args).await?,
    ))
}

pub async fn finish_and_launch(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: FinishTicketArgs,
) -> AppResult<Value> {
    use crate::chat_tools::build_multi_task_plan;
    let prep = prepare_finish(state, workspace_id, user_id, args).await?;
    if let Some(run_id) = already_launched_run(&prep.item) {
        return Ok(json!({
            "ok": true,
            "run_id": run_id,
            "ticket": {
                "provider": prep.item.provider,
                "key": prep.item.external_key,
                "title": prep.item.title,
            },
            "title": prep.title,
        }));
    }
    state
        .orch
        .store
        .ensure_project(workspace_id, "workspace fleet", 3)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = state
        .orch
        .store
        .set_auto_sync_external(workspace_id, true)
        .await;
    let mut plan = build_multi_task_plan(workspace_id, None, prep.tasks);
    plan.source = "inbox".into();
    plan.policy.auto_sync_external = true;
    stamp_inbox_llm(&mut plan, state, workspace_id).await;
    let (run, _) = state
        .orch
        .store
        .insert_accepted_plan(workspace_id, plan)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    mark_ticket_launched(
        state,
        workspace_id,
        &prep.item.provider,
        &prep.item.external_key,
        run.id,
    )
    .await?;
    let _ = state.orch.tick.tick_project(workspace_id).await;
    Ok(json!({
        "ok": true,
        "run_id": run.id,
        "ticket": {
            "provider": prep.item.provider,
            "key": prep.item.external_key,
            "title": prep.item.title,
        },
        "title": prep.title,
    }))
}

async fn stamp_inbox_llm(
    plan: &mut fleet_orchestrator::domain::WorkPlanBody,
    state: &AppState,
    workspace_id: Uuid,
) {
    let llm = crate::chat_tools::composer_or_fallback(state, workspace_id).await;
    if !llm.provider.is_empty() {
        plan.policy.llm_provider = Some(llm.provider);
    }
    if !llm.model.is_empty() {
        plan.policy.llm_model = Some(llm.model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[test]
    fn parses_linear_and_jira_keys() {
        let p = parse_ticket_ref("ENG-123").unwrap();
        assert_eq!(p.key, "ENG-123");
        assert!(p.provider.is_none());
        let p = parse_ticket_ref("jira:PROJ-9").unwrap();
        assert_eq!(p.provider.as_deref(), Some("jira"));
        assert_eq!(p.key, "PROJ-9");
    }

    /// `InboxQuery.state` is a free string so `GET /inbox?state=` cannot 500
    /// on an unknown value. Empty/whitespace is unfiltered. Parsed the same
    /// way the handler does (`axum::extract::Query`). Live HTTP filter is
    /// `inbox_get_state_query_filters_and_unknown_does_not_500` in
    /// `tests/inbox_state_query.rs` (no DATABASE_URL in `--lib`).
    #[test]
    fn inbox_query_state_unknown_does_not_fail_deserialize() {
        fn parse_query(uri: &str) -> InboxQuery {
            let uri: axum::http::Uri = uri.parse().unwrap_or_else(|e| panic!("{uri}: {e}"));
            Query::<InboxQuery>::try_from_uri(&uri)
                .unwrap_or_else(|e| {
                    panic!("InboxQuery must deserialize {uri} so GET ?state= cannot 500: {e}")
                })
                .0
        }

        let unknown = parse_query("/inbox?state=not_a_real_state");
        assert_eq!(
            inbox_state_query_filter(unknown.state.as_deref()),
            Some("not_a_real_state"),
            "unknown must stay Some (empty list), not be rejected"
        );

        for (qs, want) in [
            ("state=open", Some("open")),
            ("state=launched", Some("launched")),
            ("state=done", Some("done")),
            ("state=ignored", Some("ignored")),
            ("state=%20launched%20", Some("launched")),
            ("state=OPEN", Some("OPEN")),
            ("state=open%27%20OR%201%3D1", Some("open' OR 1=1")),
            ("state=../../../etc/passwd", Some("../../../etc/passwd")),
            ("state=", None),
            ("state=%20%20", None),
            ("", None),
        ] {
            let uri = if qs.is_empty() {
                "/inbox".to_string()
            } else {
                format!("/inbox?{qs}")
            };
            let q = parse_query(&uri);
            assert_eq!(inbox_state_query_filter(q.state.as_deref()), want, "{uri}");
        }

        assert_eq!(
            inbox_state_query_filter(None),
            None,
            "omitted state is unfiltered"
        );
        assert_eq!(inbox_state_query_filter(Some("")), None);
        assert_eq!(inbox_state_query_filter(Some("   ")), None);
        assert_eq!(
            inbox_state_query_filter(Some("not_a_real_state")),
            Some("not_a_real_state")
        );
    }

    #[test]
    fn parses_github_hash_and_url() {
        let p = parse_ticket_ref("acme/app#42").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#42");
        let p = parse_ticket_ref("https://github.com/acme/app/issues/7").unwrap();
        assert_eq!(p.key, "acme/app#7");
        let (o, r, n) = parse_github_owner_repo_num("acme/app#7").unwrap();
        assert_eq!((o, r, n), ("acme".into(), "app".into(), 7));
    }

    #[test]
    fn inbox_finish_plan_is_one_generalist() {
        let ext = ExternalRef {
            provider: "slack".into(),
            key: "C1:1".into(),
        };
        let tasks = vec![PlanTask {
            plan_task_id: "work".into(),
            title: "t".into(),
            summary: String::new(),
            spec_md: "do what the item needs".into(),
            depends_on: vec![],
            worker_profile: "generalist".into(),
            acceptance: vec!["result.md exists and is non-empty".into()],
            priority: 100,
            external_ref: Some(ext),
            timeout_s: Some(3600),
            max_retries: Some(1),
        }];
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].worker_profile, "generalist");
        assert!(!tasks.iter().any(|t| t.worker_profile.contains("review")));
    }

    #[test]
    fn parses_slack_and_notion_keys() {
        let p = parse_ticket_ref("slack:C123:171.1").unwrap();
        assert_eq!(p.provider.as_deref(), Some("slack"));
        assert_eq!(p.key, "C123:171.1");
        let p = parse_ticket_ref("notion:page-abc").unwrap();
        assert_eq!(p.provider.as_deref(), Some("notion"));
        assert_eq!(p.key, "page-abc");
    }

    /// Inbox poll marks Jira/Bitbucket items closed via `status_done`.
    /// A done-word as a substring of an open label must not hide the ticket.
    #[test]
    fn status_done_does_not_close_incomplete_or_not_done() {
        for closed in [
            "done",
            "Done",
            "closed",
            "resolved",
            "complete",
            "completed",
        ] {
            assert!(status_done(closed), "expected closed: {closed}");
        }
        for open in [
            "",
            "open",
            "in progress",
            "todo",
            "incomplete",
            "Incomplete",
            "not done",
            "Not Done",
            "reopened",
        ] {
            assert!(!status_done(open), "expected open: {open:?}");
        }
    }

    /// Inbox poll close-detection (tested-behavior): negated done-words
    /// (`not completed` / `not closed` / `not resolved`) stay open.
    #[test]
    fn status_done_does_not_close_not_completed_not_closed_or_not_resolved() {
        for closed in ["completed", "closed", "resolved"] {
            assert!(status_done(closed), "expected closed: {closed}");
        }
        for open in [
            "not completed",
            "Not Completed",
            "not closed",
            "Not Closed",
            "not resolved",
            "Not Resolved",
        ] {
            assert!(!status_done(open), "expected open: {open:?}");
        }
    }

    fn production_src() -> &'static str {
        let src = include_str!("tickets.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\nasync fn ")
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn inbox_item(workspace_id: Uuid) -> TicketInboxItem {
        let now = Utc::now();
        TicketInboxItem {
            id: Uuid::new_v4(),
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-1".into(),
            title: "from another workspace".into(),
            url: None,
            state: "open".into(),
            summary: String::new(),
            run_id: None,
            last_seen_at: now,
            created_at: now,
        }
    }

    /// Knowing a ticket id from workspace A must not finish or monitor it via B.
    /// Helper units only — live HTTP is `finish_or_monitor_ticket_in_another_workspace_is_403`
    /// in `tests/http_api.rs` (no DATABASE_URL in `--lib`).
    #[test]
    fn finish_or_monitor_ticket_in_another_workspace_is_403() {
        let err = require_workspace_role(None).expect_err("non-member must fail");
        assert!(
            matches!(&err, AppError::Forbidden(m) if m == "not a member of this workspace"),
            "{err:?}"
        );
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
        assert_eq!(
            require_workspace_role(Some("member".into())).expect("member"),
            "member"
        );

        let home = Uuid::new_v4();
        let other = Uuid::new_v4();
        let item = inbox_item(home);
        let err = require_ticket_workspace(&item, other)
            .expect_err("ticket from another workspace must fail");
        assert!(
            matches!(&err, AppError::Forbidden(m) if m == "ticket is not in this workspace"),
            "{err:?}"
        );
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
        require_ticket_workspace(&item, home).expect("same workspace is allowed");
    }

    fn item_with(state: &str, run_id: Option<Uuid>) -> TicketInboxItem {
        let mut item = inbox_item(Uuid::new_v4());
        item.state = state.into();
        item.run_id = run_id;
        item
    }

    /// Open tickets have no run; launched (or any row that already has a run)
    /// must reuse that run_id so a second Finish cannot insert another plan.
    #[test]
    fn already_launched_run_is_the_existing_run_only() {
        let run = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert_eq!(already_launched_run(&item_with("open", None)), None);
        assert_eq!(already_launched_run(&item_with("ignored", None)), None);
        assert_eq!(
            already_launched_run(&item_with("launched", Some(run))),
            Some(run)
        );
        assert_eq!(
            already_launched_run(&item_with("Launched", Some(run))),
            Some(run)
        );
        assert_eq!(
            already_launched_run(&item_with("open", Some(run))),
            Some(run),
            "any run_id means already launched — fail closed"
        );
        assert_eq!(
            already_launched_run(&item_with("ignored", Some(run))),
            Some(run)
        );
        assert_eq!(
            already_launched_run(&item_with("launched", None)),
            None,
            "launched without a run_id has nothing to reuse"
        );
        assert_ne!(
            already_launched_run(&item_with("launched", Some(run))),
            Some(other),
            "must not invent a second run id"
        );
    }

    fn assert_unknown_provider_400(raw: &str, err: AppError) {
        match err {
            AppError::BadRequest(msg) => {
                let lower = msg.to_ascii_lowercase();
                assert!(
                    lower.contains("unknown"),
                    "{raw:?} must name the reject, got {msg}"
                );
                assert!(
                    lower.contains("provider"),
                    "{raw:?} must mention provider, got {msg}"
                );
                assert_eq!(
                    AppError::bad_request(msg).into_response().status(),
                    StatusCode::BAD_REQUEST,
                    "{raw:?}"
                );
            }
            other => panic!("{raw:?} must be 400 BadRequest, not {other:?}"),
        }
    }

    /// Names not in TASK_PROVIDERS are 400 on finish and monitor (HTTP +
    /// chat `monitor_tickets`). Silent filter would still upsert auto_launch
    /// or insert a run.
    ///
    /// Helper + source-order lock only. Live POST /inbox, /inbox/finish, and
    /// chat `execute_tool` paths are in `tests/http_api.rs` (`-- unknown_provider`).
    #[test]
    fn unknown_provider_is_400_and_does_not_enable_auto_launch_or_insert_a_run() {
        for raw in [
            "not_a_real_provider",
            "openai",
            "grok",
            "gmail",
            "http",
            "TASK_PROVIDERS",
            "githubb",
            " jira2 ",
        ] {
            assert_unknown_provider_400(raw, require_task_provider(raw).expect_err(raw));
        }
        for known in TASK_PROVIDERS {
            assert_eq!(require_task_provider(known).expect(known), *known);
            assert_eq!(
                require_task_provider(&known.to_ascii_uppercase()).expect(known),
                *known
            );
        }
        assert_eq!(require_task_provider(" GitHub ").expect("trim"), "github");

        // Chat-tool / HTTP monitor body: same MonitorArgs, unknown must 400
        // even when auto_launch is requested.
        let args: MonitorArgs = serde_json::from_value(json!({
            "enabled": true,
            "providers": ["not_a_real_provider"],
            "auto_launch": true,
        }))
        .expect("chat-tool MonitorArgs shape");
        assert_eq!(args.auto_launch, Some(true));
        assert_unknown_provider_400(
            "not_a_real_provider",
            require_requested_task_providers(args.providers.as_deref().unwrap_or(&[]))
                .expect_err("monitor args with unknown provider"),
        );

        let mixed =
            require_requested_task_providers(&["github".into(), "not_a_real_provider".into()])
                .expect_err("mixed list must not keep github and drop the typo");
        assert_unknown_provider_400("github+not_a_real_provider", mixed);

        let only_unknown =
            require_requested_task_providers(&["not_a_real_provider".into(), " ".into()])
                .expect_err("unknown-only monitor list");
        assert_unknown_provider_400("not_a_real_provider", only_unknown);

        let ok =
            require_requested_task_providers(&["linear".into(), " GitHub ".into(), String::new()])
                .expect("known list");
        assert_eq!(ok, vec!["github".to_string(), "linear".to_string()]);

        let src = production_src();
        let mon = fn_src(src, "pub async fn monitor_tickets(");
        let reject_at = mon
            .find("resolve_monitor_providers(")
            .or_else(|| mon.find("require_requested_task_providers("))
            .unwrap_or_else(|| {
                panic!("monitor_tickets must reject unknown/empty providers before mutate:\n{mon}")
            });
        let upsert_at = mon
            .find("upsert_monitor(")
            .unwrap_or_else(|| panic!("monitor_tickets still upserts a monitor row:\n{mon}"));
        assert!(
            reject_at < upsert_at,
            "unknown provider must 400 before upsert_monitor (auto_launch write):\n{mon}"
        );
        if let Some(sync_at) = mon.find("set_auto_sync_external(") {
            assert!(
                reject_at < sync_at,
                "unknown provider must 400 before set_auto_sync_external:\n{mon}"
            );
        }
        assert!(
            !mon.contains(".filter(|p| TASK_PROVIDERS.contains"),
            "unknown providers must 400, not be silently filtered:\n{mon}"
        );

        let finish = fn_src(src, "async fn resolve_ticket(");
        let finish_reject = finish.find("require_task_provider(").unwrap_or_else(|| {
            panic!("finish resolve_ticket must use the same unknown-provider reject:\n{finish}")
        });
        if let Some(seed_at) = finish.find("upsert_inbox(") {
            assert!(
                finish_reject < seed_at,
                "unknown provider must 400 before seeding an inbox row:\n{finish}"
            );
        }

        let launch = fn_src(src, "pub async fn finish_and_launch(");
        let prep_at = launch.find("prepare_finish(").unwrap_or_else(|| {
            panic!("finish_and_launch must prepare_finish (provider reject):\n{launch}")
        });
        if let Some(insert_at) = launch.find("insert_accepted_plan(") {
            assert!(
                prep_at < insert_at,
                "unknown provider must 400 before insert_accepted_plan:\n{launch}"
            );
        }

        let http = fn_src(src, "async fn http_monitor(");
        let member_at = http
            .find("require_member(&state, &jar, workspace_id)")
            .unwrap_or_else(|| panic!("http_monitor must require_member:\n{http}"));
        let call_at = http.find("monitor_tickets(").unwrap_or_else(|| {
            panic!("http_monitor must call monitor_tickets (shared reject):\n{http}")
        });
        assert!(
            member_at < call_at,
            "http_monitor must require_member before monitor_tickets"
        );
        if let Some(sync_at) = http.find("set_auto_sync_external(") {
            assert!(
                call_at < sync_at,
                "http_monitor must not enable auto_sync before monitor_tickets can 400:\n{http}"
            );
        }

        // Chat tool deserializes MonitorArgs and calls the same monitor_tickets.
        let chat = include_str!("chat_tools.rs");
        let start = chat
            .find("\"monitor_tickets\" =>")
            .unwrap_or_else(|| panic!("chat tool dispatch must have a monitor_tickets arm"));
        let rest = &chat[start..];
        let end = rest.find("\"list_tickets\" =>").unwrap_or(400);
        let tool = &rest[..end];
        assert!(
            tool.contains("crate::tickets::monitor_tickets(")
                || tool.contains("tickets::monitor_tickets("),
            "chat monitor_tickets tool must call tickets::monitor_tickets:\n{tool}"
        );
        assert!(
            !tool.contains("upsert_monitor("),
            "chat tool must not upsert a monitor itself (would skip the reject):\n{tool}"
        );
        assert!(
            !tool.contains("insert_accepted_plan("),
            "chat monitor_tickets must not insert a run:\n{tool}"
        );
    }

    fn assert_no_listen_source_400(err: AppError, label: &str) {
        match err {
            AppError::BadRequest(msg) => {
                let lower = msg.to_ascii_lowercase();
                assert!(
                    lower.contains("listen") || lower.contains("connected"),
                    "{label} must name the missing source, got {msg}"
                );
                assert!(
                    !lower.contains("unknown"),
                    "{label} is empty, not unknown provider: {msg}"
                );
                assert_eq!(
                    AppError::bad_request(msg).into_response().status(),
                    StatusCode::BAD_REQUEST,
                    "{label}"
                );
            }
            other => panic!("{label} must be 400 BadRequest, not {other:?}"),
        }
    }

    /// Empty / blank / unknown monitor provider lists fail closed. Empty
    /// request falls back to connected sources; enabling with nothing
    /// connected is 400 so auto_launch cannot be upserted. Live POST
    /// /inbox and chat `monitor_tickets` are in `tests/http_api.rs`
    /// (`-- empty_or_invalid_monitor`).
    #[test]
    fn empty_or_invalid_monitor_provider_list_fails_closed() {
        for (label, requested) in [
            ("empty", vec![]),
            ("blanks", vec!["".into(), "  ".into(), "\t".into()]),
        ] {
            assert_no_listen_source_400(
                resolve_monitor_providers(&requested, &[], true).expect_err(label),
                label,
            );
        }

        // UI Start monitor + auto_launch with no providers field.
        let start: MonitorArgs = serde_json::from_value(json!({
            "enabled": true,
            "auto_launch": true,
        }))
        .expect("start-monitor + auto_launch shape");
        assert_eq!(start.auto_launch, Some(true));
        assert!(start.providers.as_deref().unwrap_or(&[]).is_empty());
        assert_no_listen_source_400(
            resolve_monitor_providers(
                start.providers.as_deref().unwrap_or(&[]),
                &[],
                start.enabled.unwrap_or(true),
            )
            .expect_err("start + auto_launch + no connect"),
            "start+auto_launch",
        );

        let empty_list: MonitorArgs = serde_json::from_value(json!({
            "enabled": true,
            "providers": [],
            "auto_launch": true,
        }))
        .expect("empty providers + auto_launch");
        assert_no_listen_source_400(
            resolve_monitor_providers(
                empty_list.providers.as_deref().unwrap_or(&[]),
                &[],
                empty_list.enabled.unwrap_or(true),
            )
            .expect_err("empty providers + auto_launch"),
            "empty providers + auto_launch",
        );

        // Not a tautology: empty request + a connected source is allowed.
        let fallback =
            resolve_monitor_providers(&[], &["github".into()], true).expect("connected fallback");
        assert_eq!(fallback, vec!["github".to_string()]);

        // Explicit known name still works with no Connect (poll owns not_connected).
        let explicit =
            resolve_monitor_providers(&[" GitHub ".into()], &[], true).expect("explicit github");
        assert_eq!(explicit, vec!["github".to_string()]);

        // Invalid name is 400 even when something is connected — must not
        // keep github and drop the typo, then arm auto_launch.
        let only_typo =
            resolve_monitor_providers(&["not_a_real_provider".into()], &["github".into()], true)
                .expect_err("invalid name");
        assert_unknown_provider_400("not_a_real_provider", only_typo);

        let mixed =
            resolve_monitor_providers(&["github".into(), "not_a_real_provider".into()], &[], true)
                .expect_err("mixed must not keep github");
        assert_unknown_provider_400("github+not_a_real_provider", mixed);

        // Stop monitor (enabled false) with an empty list is allowed.
        let stopped = resolve_monitor_providers(&[], &[], false).expect("stop");
        assert!(stopped.is_empty());
        assert!(
            !persist_monitor_auto_launch(&stopped, Some(true)),
            "disabled empty + auto_launch must not persist auto_launch"
        );
        assert!(persist_monitor_auto_launch(&["github".into()], Some(true)));
        assert!(!persist_monitor_auto_launch(
            &["github".into()],
            Some(false)
        ));
        assert!(!persist_monitor_auto_launch(&["github".into()], None));
    }
}
