//! Inbox ingest: `@fleet monitor` watches connected sources; `@fleet finish`
//! launches a specialist plan for one item.
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
pub const TASK_PROVIDERS: &[&str] = &[
    "github", "gitlab", "jira", "linear", "slack", "notion", "trello", "asana",
];
/// Auto-launch only these — chat/page queries, not every old Jira issue.
pub const AUTO_LAUNCH_PROVIDERS: &[&str] = &["slack", "notion", "teams"];

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

pub async fn monitor_tickets(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: MonitorArgs,
) -> AppResult<Value> {
    let enabled = args.enabled.unwrap_or(true);
    let connected = connected_task_providers(state, workspace_id).await?;
    let mut providers: Vec<String> = args
        .providers
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| TASK_PROVIDERS.contains(&p.as_str()))
        .collect();
    if providers.is_empty() {
        providers = connected.clone();
    }
    if enabled && providers.is_empty() {
        return Err(AppError::bad_request(
            "no listen source connected — open Integrations and Connect Slack, Notion, GitHub, Jira, Linear, … then say @fleet monitor",
        ));
    }
    providers.sort();
    providers.dedup();

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
    let auto_launch = args.auto_launch.unwrap_or(false);

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

pub async fn prepare_finish(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
    args: FinishTicketArgs,
) -> AppResult<PreparedFinish> {
    let item = resolve_ticket(state, workspace_id, &args).await?;
    let live = fetch_ticket_details(state, workspace_id, user_id, &item).await;
    let spec = format!(
        "{}\n\n# I/O\n\
         Source: `{provider}` `{key}`.\n\
         Use `use_integration` to read/write any connected app (Slack, Notion, GitHub, git, …). \
         Tokens stay on the control plane.\n\
         When you finish, write `result.md`. The control plane will also reply on this source \
         (Slack thread, Notion page, or ticket comment).",
        ticket_spec_md(&item, live.as_ref()),
        provider = item.provider,
        key = item.external_key,
    );
    let title = format!("{}: {}", item.external_key, item.title);
    let ext = ExternalRef {
        provider: item.provider.clone(),
        key: item.external_key.clone(),
    };
    let tasks = vec![
        PlanTask {
            plan_task_id: "impl".into(),
            title: title.clone(),
            summary: spec.chars().take(280).collect(),
            spec_md: spec.clone(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: Some(ext.clone()),
            timeout_s: Some(3600),
            max_retries: Some(1),
        },
        PlanTask {
            plan_task_id: "rev".into(),
            title: format!("Review {}", item.external_key),
            summary: format!("Review the implementation of {}", item.external_key),
            spec_md: format!(
                "Review `{key}` ({title}). Check the work in `work/` against the ticket. \
                 If it is not acceptable, call `request_changes`.\n\n{spec}",
                key = item.external_key,
                title = item.title,
            ),
            depends_on: vec!["impl".into()],
            worker_profile: "reviewer".into(),
            acceptance: vec![],
            priority: 90,
            external_ref: Some(ext),
            timeout_s: Some(3600),
            max_retries: Some(1),
        },
    ];
    Ok(PreparedFinish { item, tasks, title })
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

pub async fn poll_workspace(
    state: &AppState,
    workspace_id: Uuid,
    force: bool,
) -> AppResult<Value> {
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
            "github" => match poll_github(state, workspace_id, &monitor.github_repos, &caller).await
            {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("github: {e}")),
            },
            "jira" => match poll_jira(state, workspace_id, monitor.jira_jql.as_deref(), &caller).await
            {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("jira: {e}")),
            },
            "linear" => match poll_linear(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("linear: {e}")),
            },
            "gitlab" => match poll_gitlab(state, workspace_id, &caller).await {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("gitlab: {e}")),
            },
            "slack" => match poll_slack(state, workspace_id, &monitor.slack_channels, &caller).await
            {
                Ok(n) => seen += n,
                Err(e) => errors.push(format!("slack: {e}")),
            },
            "notion" => {
                match poll_notion(state, workspace_id, &monitor.notion_databases, &caller).await {
                    Ok(n) => seen += n,
                    Err(e) => errors.push(format!("notion: {e}")),
                }
            }
            _ => {}
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
        .filter(|i| {
            AUTO_LAUNCH_PROVIDERS.contains(&i.provider.as_str())
                && i.run_id.is_none()
                && i.created_at >= cutoff
        })
        .take(3)
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

async fn connected_task_providers(
    state: &AppState,
    workspace_id: Uuid,
) -> AppResult<Vec<String>> {
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
        let desc = i
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let state_type = i
            .get("state_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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

fn status_done(status: &str) -> bool {
    let s = status.to_ascii_lowercase();
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
            "no open tickets — say @fleet monitor first (and Connect GitHub / Jira / Linear), or pass a key like ENG-123",
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
    let args = match item.provider.as_str() {
        "github" => {
            let (owner, repo, number) = parse_github_owner_repo_num(&item.external_key)?;
            json!({ "owner": owner, "repo": repo, "number": number })
        }
        "jira" => json!({ "key": item.external_key }),
        "linear" => json!({ "identifier": item.external_key }),
        _ => return None,
    };
    let op = match item.provider.as_str() {
        "jira" => "get_issue",
        _ => "get_issue",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linear_and_jira_keys() {
        let p = parse_ticket_ref("ENG-123").unwrap();
        assert_eq!(p.key, "ENG-123");
        assert!(p.provider.is_none());
        let p = parse_ticket_ref("jira:PROJ-9").unwrap();
        assert_eq!(p.provider.as_deref(), Some("jira"));
        assert_eq!(p.key, "PROJ-9");
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
    fn parses_slack_and_notion_keys() {
        let p = parse_ticket_ref("slack:C123:171.1").unwrap();
        assert_eq!(p.provider.as_deref(), Some("slack"));
        assert_eq!(p.key, "C123:171.1");
        let p = parse_ticket_ref("notion:page-abc").unwrap();
        assert_eq!(p.provider.as_deref(), Some("notion"));
        assert_eq!(p.key, "page-abc");
    }
}
