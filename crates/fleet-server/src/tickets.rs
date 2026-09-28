//! Ticket inbox: poll connected apps, finish into a fleet run.

use std::time::Duration;

use chrono::Utc;
use fleet_auth::{AppError, AppResult};
use fleet_db::repos::integrations as integ_repo;
use fleet_db::repos::tickets::{self as ticket_repo, UpsertInbox, UpsertMonitor};
use fleet_db::TicketInboxItem;
use fleet_integrations::proxy::{ProxyCaller, ProxyResult};
use fleet_orchestrator::domain::{ExternalRef, PlanTask};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::inbox_sources::{self, InboxKind, DEFAULT_LAUNCH_LIMIT, DEFAULT_POLL_LIMIT};
use crate::state::AppState;

pub const POLL_MIN_INTERVAL: Duration = Duration::from_secs(60);

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
    pub gitlab_projects: Option<Vec<String>>,
    #[serde(default)]
    pub gitea_repos: Option<Vec<String>>,
    #[serde(default)]
    pub linear_teams: Option<Vec<String>>,
    #[serde(default)]
    pub trello_boards: Option<Vec<String>>,
    #[serde(default)]
    pub auto_launch: Option<bool>,
    #[serde(default)]
    pub follow_connected: Option<bool>,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub launch_limit: Option<i32>,
    #[serde(default)]
    pub poll_limit: Option<i32>,
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
    /// When true, a pasted provider+key that is not in this workspace is
    /// inserted as an open stub. Default is look-up only.
    #[serde(default)]
    pub create_if_missing: Option<bool>,
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
        if inbox_sources::is_inbox_provider(&provider) {
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
        let num = num.trim();
        // Whole suffix must be digits. `stack#5cc319fa` is not issue 5.
        if repo.contains('/') && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
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
    let existing = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let enabled = args.enabled.unwrap_or(true);
    let connected = connected_task_providers(state, workspace_id).await?;

    let follow_connected = args.follow_connected.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|m| m.follow_connected)
            .unwrap_or(true)
    });
    let providers: Vec<String> = match args.providers {
        Some(p) => p
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| inbox_sources::is_inbox_provider(s))
            .collect(),
        None => existing
            .as_ref()
            .map(|m| m.providers.clone())
            .unwrap_or_default(),
    };
    let watching = inbox_sources::resolve_watching(follow_connected, &providers, &connected);
    if enabled && watching.is_empty() {
        return Err(AppError::bad_request(
            "no listen source connected — open Integrations, Connect an app, then Start monitor on Inbox",
        ));
    }

    let existing_filters = existing
        .as_ref()
        .map(|m| m.filters.clone())
        .unwrap_or_else(|| json!({}));
    let mut filters = inbox_sources::merge_filters(
        &existing_filters,
        args.filters.as_ref(),
        args.github_repos.as_deref(),
        args.jira_jql.as_deref(),
        args.slack_channels.as_deref(),
        args.notion_databases.as_deref(),
        args.gitlab_projects.as_deref(),
        args.linear_teams.as_deref(),
        args.trello_boards.as_deref(),
    );
    if let Some(repos) = args.gitea_repos.as_deref() {
        let repos: Vec<&str> = repos.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        filters["gitea"] = json!({ "repos": repos });
    }
    let github_repos = inbox_sources::github_repos(&filters);
    let jira_jql = inbox_sources::jira_jql(&filters);
    let slack_channels = inbox_sources::slack_channels(&filters);
    let notion_databases = inbox_sources::notion_databases(&filters);

    let auto_launch = args
        .auto_launch
        .unwrap_or_else(|| existing.as_ref().map(|m| m.auto_launch).unwrap_or(false));
    let launch_limit = inbox_sources::clamp_launch_limit(args.launch_limit.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|m| m.launch_limit)
            .unwrap_or(DEFAULT_LAUNCH_LIMIT)
    }));
    let poll_limit = inbox_sources::clamp_poll_limit(args.poll_limit.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|m| m.poll_limit)
            .unwrap_or(DEFAULT_POLL_LIMIT)
    }));

    let monitor = ticket_repo::upsert_monitor(
        &state.db,
        &UpsertMonitor {
            workspace_id,
            enabled,
            providers,
            github_repos,
            jira_jql,
            slack_channels,
            notion_databases,
            auto_launch,
            follow_connected,
            filters,
            launch_limit,
            poll_limit,
            hook_token: existing
                .as_ref()
                .map(|m| m.hook_token.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(new_hook_token),
            started_by: Some(user_id),
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    if enabled
        && watching
            .iter()
            .any(|p| matches!(inbox_sources::kind_for(p), InboxKind::Ticket))
    {
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
    let tickets = ticket_repo::list_inbox(&state.db, workspace_id, None, Some(200))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(monitor_json(
        &monitor,
        &connected,
        &state.integrations.api_public_base,
        poll,
        Some(tickets_json(&tickets)),
    ))
}

pub async fn list_tickets(
    state: &AppState,
    workspace_id: Uuid,
    state_filter: Option<&str>,
) -> AppResult<Value> {
    let monitor = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let tickets = ticket_repo::list_inbox(&state.db, workspace_id, state_filter, Some(200))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let connected = connected_task_providers(state, workspace_id).await?;
    let mut body = json!({
        "tickets": tickets_json(&tickets),
        "count": tickets.len(),
    });
    if let Some(m) = monitor.as_ref() {
        body["monitor"] = monitor_json(
            m,
            &connected,
            &state.integrations.api_public_base,
            None,
            None,
        );
    } else {
        body["monitor"] = Value::Null;
    }
    Ok(body)
}

fn hook_url_template(api_base: &str, token: &str) -> String {
    format!(
        "{}/api/integrations/webhooks/{{provider}}/{token}",
        api_base.trim_end_matches('/')
    )
}

fn new_hook_token() -> String {
    format!(
        "{}{}",
        Uuid::new_v4().as_simple(),
        Uuid::new_v4().as_simple()
    )
}

fn monitor_json(
    monitor: &fleet_db::TicketMonitor,
    connected: &[String],
    api_base: &str,
    poll: Option<Value>,
    tickets: Option<Value>,
) -> Value {
    let watching =
        inbox_sources::resolve_watching(monitor.follow_connected, &monitor.providers, connected);
    let mut out = json!({
        "enabled": monitor.enabled,
        "follow_connected": monitor.follow_connected,
        "providers": monitor.providers,
        "watching": watching,
        "filters": monitor.filters,
        "github_repos": inbox_sources::github_repos(&monitor.filters),
        "jira_jql": inbox_sources::jira_jql(&monitor.filters),
        "slack_channels": inbox_sources::slack_channels(&monitor.filters),
        "notion_databases": inbox_sources::notion_databases(&monitor.filters),
        "gitlab_projects": inbox_sources::string_list(&monitor.filters, "gitlab", "projects"),
        "gitea_repos": inbox_sources::gitea_repos(&monitor.filters),
        "linear_teams": inbox_sources::string_list(&monitor.filters, "linear", "teams"),
        "trello_boards": inbox_sources::string_list(&monitor.filters, "trello", "boards"),
        "auto_launch": monitor.auto_launch,
        "launch_limit": monitor.launch_limit,
        "poll_limit": monitor.poll_limit,
        "hook_url": hook_url_template(api_base, &monitor.hook_token),
        "last_poll_at": monitor.last_poll_at,
        "last_error": monitor.last_error,
    });
    if let Some(poll) = poll {
        out["poll"] = poll;
    }
    if let Some(tickets) = tickets {
        out["tickets"] = tickets;
    }
    out
}

pub struct PreparedFinish {
    pub item: TicketInboxItem,
    pub tasks: Vec<PlanTask>,
    pub title: String,
}

fn require_workspace_role(role: Option<String>) -> AppResult<String> {
    role.ok_or_else(|| AppError::forbidden("not a member of this workspace"))
}

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
    if item.state == "launched" || item.run_id.is_some() {
        return Err(AppError::conflict_code(
            "already_launched",
            "ticket already launched",
        ));
    }
    if item.state != "open" {
        return Err(AppError::conflict_code("not_open", "ticket is not open"));
    }
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
    let tasks = inbox_plan_tasks(&title, &spec, ext);
    Ok(PreparedFinish { item, tasks, title })
}

/// Plan follows the inbox kind: tickets get work+review; chat/docs get one ops task.
pub fn inbox_plan_tasks(title: &str, spec: &str, ext: ExternalRef) -> Vec<PlanTask> {
    match inbox_sources::kind_for(&ext.provider) {
        InboxKind::Ticket => ticket_work_review_plan(title, spec, ext),
        InboxKind::Message | InboxKind::Page => {
            let profile = match inbox_sources::kind_for(&ext.provider) {
                InboxKind::Page => "analysis",
                _ => "ops",
            };
            vec![single_inbox_task(title, spec, ext, profile)]
        }
    }
}

fn single_inbox_task(title: &str, spec: &str, ext: ExternalRef, profile: &str) -> PlanTask {
    PlanTask {
        plan_task_id: "handle".into(),
        title: title.to_string(),
        summary: spec.chars().take(280).collect(),
        spec_md: spec.to_string(),
        depends_on: vec![],
        worker_profile: profile.into(),
        acceptance: vec!["result.md exists and is non-empty".into()],
        priority: 100,
        external_ref: Some(ext),
        timeout_s: Some(1800),
        max_retries: Some(1),
    }
}

fn ticket_work_review_plan(title: &str, spec: &str, ext: ExternalRef) -> Vec<PlanTask> {
    let work_spec = spec.to_string();
    let review_spec = format!(
        "Review the work for `{key}` on `{provider}`.\n\
         Read `handoff/work.md` and `work/`.\n\
         Use `use_integration` if you need the live ticket or to comment.\n\
         If it is incomplete, `request_changes`. When it is good, write `result.md`.\n\n\
         Original spec:\n{spec}",
        key = ext.key,
        provider = ext.provider,
        spec = spec.chars().take(2000).collect::<String>(),
    );
    vec![
        PlanTask {
            plan_task_id: "work".into(),
            title: title.to_string(),
            summary: work_spec.chars().take(280).collect(),
            spec_md: work_spec,
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec!["result.md exists and is non-empty".into()],
            priority: 100,
            external_ref: Some(ext.clone()),
            timeout_s: Some(3600),
            max_retries: Some(2),
        },
        PlanTask {
            plan_task_id: "review".into(),
            title: format!("Review {title}"),
            summary: "Review the implementer's result".into(),
            spec_md: review_spec,
            depends_on: vec!["work".into()],
            worker_profile: "reviewer".into(),
            acceptance: vec!["result.md exists and is non-empty".into()],
            priority: 80,
            external_ref: Some(ext),
            timeout_s: Some(1800),
            max_retries: Some(1),
        },
    ]
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
    let claimed = ticket_repo::mark_launched(&state.db, workspace_id, provider, key, run_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if claimed.is_none() {
        return Err(AppError::conflict("ticket already launched"));
    }
    Ok(())
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
    let connected = connected_task_providers(state, workspace_id)
        .await
        .unwrap_or_default();
    let watching =
        inbox_sources::resolve_watching(monitor.follow_connected, &monitor.providers, &connected);
    let item_limit = inbox_sources::clamp_poll_limit(monitor.poll_limit) as usize;
    let target_limit = inbox_sources::target_limit(monitor.poll_limit);
    let filters = &monitor.filters;
    let mut errors: Vec<String> = Vec::new();
    let mut seen = 0usize;
    for provider in &watching {
        let n = match provider.as_str() {
            "github" => {
                poll_github(
                    state,
                    workspace_id,
                    &inbox_sources::github_repos(filters),
                    &caller,
                    item_limit,
                    target_limit,
                )
                .await
            }
            "jira" => {
                poll_jira(
                    state,
                    workspace_id,
                    inbox_sources::jira_jql(filters).as_deref(),
                    &caller,
                    item_limit,
                )
                .await
            }
            "linear" => {
                poll_linear(
                    state,
                    workspace_id,
                    &caller,
                    item_limit,
                    &inbox_sources::linear_teams(filters),
                )
                .await
            }
            "gitea" => {
                poll_gitea(
                    state,
                    workspace_id,
                    &caller,
                    item_limit,
                    target_limit,
                    &inbox_sources::gitea_repos(filters),
                )
                .await
            }
            "gitlab" => {
                poll_gitlab(
                    state,
                    workspace_id,
                    &caller,
                    item_limit,
                    target_limit,
                    &inbox_sources::gitlab_projects(filters),
                )
                .await
            }
            "slack" => {
                poll_slack(
                    state,
                    workspace_id,
                    &inbox_sources::slack_channels(filters),
                    &caller,
                    item_limit,
                    target_limit,
                )
                .await
            }
            "notion" => {
                poll_notion(
                    state,
                    workspace_id,
                    &inbox_sources::notion_databases(filters),
                    &caller,
                    item_limit,
                    target_limit,
                )
                .await
            }
            "trello" => {
                poll_trello(
                    state,
                    workspace_id,
                    &caller,
                    item_limit,
                    target_limit,
                    &inbox_sources::trello_boards(filters),
                )
                .await
            }
            "asana" => poll_asana(state, workspace_id, &caller, item_limit).await,
            "teams" => poll_teams(state, workspace_id, &caller, item_limit, target_limit).await,
            "bitbucket" => {
                poll_bitbucket(state, workspace_id, &caller, item_limit, target_limit).await
            }
            "confluence" => poll_confluence(state, workspace_id, &caller, item_limit).await,
            _ => Err("no inbox adapter".into()),
        };
        match n {
            Ok(n) => seen += n,
            Err(e) => errors.push(format!("{provider}: {e}")),
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
    let drive = drive_inbox(state, workspace_id, Some(&monitor), true).await;
    Ok(json!({
        "watching": watching,
        "seen": seen,
        "errors": errors,
        "reopened": drive.reopened,
        "launched": drive.launched,
        "launch_errors": drive.errors,
    }))
}

struct DriveReport {
    reopened: usize,
    launched: usize,
    errors: Vec<String>,
}

/// Retry dead launched runs (poll only), then start open tickets.
async fn drive_inbox(
    state: &AppState,
    workspace_id: Uuid,
    monitor: Option<&fleet_db::TicketMonitor>,
    reconcile_launched: bool,
) -> DriveReport {
    let mut out = DriveReport {
        reopened: 0,
        launched: 0,
        errors: Vec::new(),
    };
    let owned;
    let monitor = match monitor {
        Some(m) => m,
        None => {
            owned = match ticket_repo::get_monitor(&state.db, workspace_id).await {
                Ok(Some(m)) => m,
                _ => return out,
            };
            &owned
        }
    };
    if !monitor.enabled {
        return out;
    }

    if reconcile_launched {
        let launched_rows = match ticket_repo::list_inbox(
            &state.db,
            workspace_id,
            Some("launched"),
            Some(64),
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                out.errors.push(format!("reconcile: {e}"));
                return out;
            }
        };
        for item in launched_rows {
            let Some(run_id) = item.run_id else {
                let stale = Utc::now().signed_duration_since(item.last_seen_at)
                    > chrono::Duration::minutes(2);
                if stale
                    && ticket_repo::reopen_ticket(
                        &state.db,
                        workspace_id,
                        &item.provider,
                        &item.external_key,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    out.reopened += 1;
                }
                continue;
            };
            let run = state
                .orch
                .store
                .get_run(workspace_id, run_id)
                .await
                .ok()
                .flatten();
            let status = run.as_ref().map(|r| r.status.as_str()).unwrap_or("missing");
            match status {
                "failed" | "cancelled" | "missing" => {
                    if ticket_repo::reopen_ticket(
                        &state.db,
                        workspace_id,
                        &item.provider,
                        &item.external_key,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                    {
                        out.reopened += 1;
                    }
                }
                "completed" => {
                    let _ = ticket_repo::mark_inbox_done(
                        &state.db,
                        workspace_id,
                        &item.provider,
                        &item.external_key,
                    )
                    .await;
                }
                _ => {}
            }
        }
    }

    if !monitor.auto_launch {
        return out;
    }
    let user_id = monitor.started_by.unwrap_or(Uuid::nil());
    let launch_cap = inbox_sources::clamp_launch_limit(monitor.launch_limit) as i64;
    let open = match ticket_repo::list_inbox(
        &state.db,
        workspace_id,
        Some("open"),
        Some(launch_cap),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            out.errors.push(format!("list open: {e}"));
            return out;
        }
    };
    for item in open.into_iter().filter(|i| i.run_id.is_none()) {
        match finish_and_launch(
            state,
            workspace_id,
            user_id,
            FinishTicketArgs {
                ticket: None,
                provider: Some(item.provider.clone()),
                key: Some(item.external_key.clone()),
                create_if_missing: None,
            },
        )
        .await
        {
            Ok(_) => out.launched += 1,
            Err(e) => out.errors.push(format!("{}: {e}", item.external_key)),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct IngestItem {
    pub provider: String,
    pub key: String,
    pub title: String,
    pub url: Option<String>,
    pub summary: String,
    pub closed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct IngestReport {
    pub ingested: usize,
    pub launched: usize,
}

/// Upsert webhook items and launch if the monitor is on.
pub async fn ingest_webhook_items(
    state: &AppState,
    workspace_id: Uuid,
    items: Vec<IngestItem>,
) -> AppResult<IngestReport> {
    let mut report = IngestReport::default();
    let Some(monitor) = ticket_repo::get_monitor(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    else {
        return Ok(report);
    };
    if !monitor.enabled {
        return Ok(report);
    }
    let filters = monitor.filters.clone();
    for item in items {
        if !inbox_sources::webhook_provider_allowed(
            monitor.follow_connected,
            &monitor.providers,
            &item.provider,
        ) {
            continue;
        }
        if !inbox_sources::item_matches_filters(&filters, &item.provider, &item.key) {
            continue;
        }
        ticket_repo::upsert_inbox(
            &state.db,
            &UpsertInbox {
                workspace_id,
                provider: item.provider,
                external_key: item.key,
                title: item.title,
                url: item.url,
                summary: item.summary,
                provider_closed: item.closed,
            },
        )
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
        report.ingested += 1;
    }
    if report.ingested == 0 {
        return Ok(report);
    }
    let drive = drive_inbox(state, workspace_id, Some(&monitor), false).await;
    report.launched = drive.launched;
    Ok(report)
}

pub async fn poll_due_workspaces(state: &AppState) {
    let ids = match ticket_repo::list_enabled_workspace_ids(&state.db).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "ticket monitor: list enabled failed");
            return;
        }
    };
    let mut set = tokio::task::JoinSet::new();
    for id in ids {
        while set.len() >= 4 {
            let _ = set.join_next().await;
        }
        let state = state.clone();
        set.spawn(async move {
            if let Err(e) = poll_workspace(&state, id, false).await {
                tracing::warn!(workspace_id = %id, error = %e, "ticket monitor poll failed");
            }
        });
    }
    while set.join_next().await.is_some() {}
}

async fn connected_task_providers(state: &AppState, workspace_id: Uuid) -> AppResult<Vec<String>> {
    let rows = integ_repo::list_for_workspace(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|c| c.provider.to_ascii_lowercase())
        .filter(|p| inbox_sources::is_inbox_provider(p))
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
    crate::workforce::gated_invoke(
        state,
        workspace_id,
        caller.clone(),
        provider,
        operation,
        args,
    )
    .await
}

async fn poll_github(
    state: &AppState,
    workspace_id: Uuid,
    repos: &[String],
    caller: &ProxyCaller,
    item_limit: usize,
    target_limit: usize,
) -> Result<usize, String> {
    let mut targets = repos.to_vec();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "github",
            "list_repos",
            json!({ "per_page": target_limit }),
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
    for repo in targets.iter().take(target_limit) {
        let Some((owner, name)) = repo.split_once('/') else {
            continue;
        };
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "github",
            "list_issues",
            json!({ "owner": owner, "repo": name, "state": "open", "per_page": item_limit }),
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
    item_limit: usize,
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
        json!({ "jql": jql, "max_results": item_limit }),
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
    item_limit: usize,
    teams: &[String],
) -> Result<usize, String> {
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "linear",
        "list_issues",
        json!({ "limit": item_limit }),
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
        if !inbox_sources::item_matches_filters(
            &json!({ "linear": { "teams": teams } }),
            "linear",
            &key,
        ) {
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
    item_limit: usize,
    target_limit: usize,
    projects: &[String],
) -> Result<usize, String> {
    let mut targets: Vec<String> = projects
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "gitlab",
            "list_repos",
            json!({ "per_page": target_limit }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        if let Some(arr) = listed.data.get("items").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(name) = r
                    .get("path_with_namespace")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    targets.push(name.to_string());
                }
            }
        }
    }
    if targets.is_empty() {
        return Ok(0);
    }
    let mut n = 0usize;
    for project in targets.iter().take(target_limit) {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "gitlab",
            "list_issues",
            json!({ "project": project, "state": "opened", "per_page": item_limit }),
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
        for i in items {
            let iid = i
                .get("iid")
                .or_else(|| i.get("number"))
                .and_then(|v| v.as_u64())
                .or_else(|| i.get("iid").and_then(|v| v.as_i64()).map(|n| n as u64));
            let Some(iid) = iid else {
                continue;
            };
            let path = i
                .get("project")
                .or_else(|| i.get("project_path"))
                .and_then(|v| v.as_str())
                .unwrap_or(project.as_str());
            let key = format!("{path}#{iid}");
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
    }
    Ok(n)
}

async fn poll_gitea(
    state: &AppState,
    workspace_id: Uuid,
    caller: &ProxyCaller,
    item_limit: usize,
    target_limit: usize,
    repos: &[String],
) -> Result<usize, String> {
    let mut targets: Vec<String> = repos
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "gitea",
            "list_repos",
            json!({ "per_page": target_limit }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        if let Some(arr) = listed.data.get("items").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(name) = r
                    .get("full_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    targets.push(name.to_string());
                }
            }
        }
    }
    if targets.is_empty() {
        return Ok(0);
    }
    let mut n = 0usize;
    for repo in targets.iter().take(target_limit) {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "gitea",
            "list_issues",
            json!({ "repo": repo, "state": "open", "per_page": item_limit }),
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
        for i in items {
            let number = i.get("number").and_then(|v| v.as_u64());
            let Some(number) = number else { continue };
            let key = format!("{repo}#{number}");
            let title = i
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("untitled")
                .to_string();
            ticket_repo::upsert_inbox(
                &state.db,
                &UpsertInbox {
                    workspace_id,
                    provider: "gitea".into(),
                    external_key: key,
                    title,
                    url: i.get("html_url").and_then(|v| v.as_str()).map(|s| s.into()),
                    summary: i
                        .get("body")
                        .or_else(|| i.get("description"))
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
    }
    Ok(n)
}

async fn poll_slack(
    state: &AppState,
    workspace_id: Uuid,
    channels: &[String],
    caller: &ProxyCaller,
    item_limit: usize,
    target_limit: usize,
) -> Result<usize, String> {
    let mut targets = channels.to_vec();
    if targets.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "slack",
            "list_channels",
            json!({ "limit": target_limit }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        if let Some(arr) = listed.data.get("items").and_then(|v| v.as_array()) {
            for c in arr.iter().take(target_limit) {
                if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
                    targets.push(id.to_string());
                }
            }
        }
    }
    let mut n = 0usize;
    let mut ok_channels = 0usize;
    let mut channel_errors = Vec::new();
    for channel in targets.iter().take(target_limit) {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "slack",
            "get_channel_history",
            json!({ "channel": channel, "limit": item_limit }),
        )
        .await;
        if !listed.ok {
            channel_errors.push(format!("{channel}: {}", listed.summary));
            continue;
        }
        ok_channels += 1;
        let items = listed
            .data
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for m in items {
            let Some((key, title, summary)) = inbox_sources::slack_inbox_item(channel, &m) else {
                continue;
            };
            ticket_repo::upsert_inbox(
                &state.db,
                &UpsertInbox {
                    workspace_id,
                    provider: "slack".into(),
                    external_key: key,
                    title,
                    url: None,
                    summary,
                    provider_closed: false,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    inbox_sources::slack_poll_outcome(ok_channels, n, &channel_errors)
}

async fn poll_notion(
    state: &AppState,
    workspace_id: Uuid,
    databases: &[String],
    caller: &ProxyCaller,
    item_limit: usize,
    target_limit: usize,
) -> Result<usize, String> {
    let mut n = 0usize;
    if databases.is_empty() {
        let listed = proxy_op(
            state,
            workspace_id,
            caller,
            "notion",
            "search",
            json!({ "page_size": item_limit, "filter": { "value": "page", "property": "object" } }),
        )
        .await;
        if !listed.ok {
            return Err(listed.summary);
        }
        n += upsert_notion_items(state, workspace_id, &listed.data).await?;
    } else {
        for db in databases.iter().take(target_limit) {
            let listed = proxy_op(
                state,
                workspace_id,
                caller,
                "notion",
                "query_database",
                json!({ "database_id": db, "page_size": item_limit }),
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
    item_limit: usize,
    target_limit: usize,
    board_allow: &[String],
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
    for b in json_items(&boards.data).into_iter().take(target_limit) {
        let board_id = json_text(&b, &["id", "idBoard"]);
        if board_id.is_empty() {
            continue;
        }
        if !inbox_sources::listed_hit(board_allow, &board_id) {
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
        for c in json_items(&cards.data).into_iter().take(item_limit) {
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
    item_limit: usize,
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
    for t in json_items(&listed.data).into_iter().take(item_limit) {
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
    item_limit: usize,
    target_limit: usize,
) -> Result<usize, String> {
    let chats = proxy_op(
        state,
        workspace_id,
        caller,
        "teams",
        "list_chats",
        json!({ "limit": target_limit }),
    )
    .await;
    if !chats.ok {
        return Err(chats.summary);
    }
    let mut n = 0usize;
    for chat in json_items(&chats.data).into_iter().take(target_limit) {
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
            json!({ "chat_id": chat_id, "limit": item_limit }),
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
    item_limit: usize,
    target_limit: usize,
) -> Result<usize, String> {
    let repos = proxy_op(
        state,
        workspace_id,
        caller,
        "bitbucket",
        "list_repos",
        json!({ "per_page": target_limit }),
    )
    .await;
    if !repos.ok {
        return Err(repos.summary);
    }
    let mut n = 0usize;
    for r in json_items(&repos.data).into_iter().take(target_limit) {
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
        for i in json_items(&listed.data).into_iter().take(item_limit) {
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
    item_limit: usize,
) -> Result<usize, String> {
    let listed = proxy_op(
        state,
        workspace_id,
        caller,
        "confluence",
        "search",
        json!({ "query": "type=page", "limit": item_limit }),
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

pub fn status_looks_done(status: &str) -> bool {
    status_done(status)
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
    let explicit_provider = args
        .provider
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let provider = explicit_provider
        .clone()
        .or_else(|| parsed.as_ref().and_then(|p| p.provider.clone()));
    if let Some(p) = provider.as_deref() {
        if !inbox_sources::is_inbox_provider(p) {
            return Err(AppError::bad_request(format!(
                "unknown inbox provider `{p}`"
            )));
        }
    }
    let key = if explicit_provider.is_some() {
        args.key
            .as_deref()
            .or(args.ticket.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    } else {
        parsed.as_ref().map(|p| p.key.clone())
    };

    if let (Some(provider), Some(key)) = (provider.as_deref(), key.as_deref()) {
        if let Some(hit) = ticket_repo::find_inbox(&state.db, workspace_id, provider, key)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
        {
            return Ok(hit);
        }
        if args.create_if_missing.unwrap_or(false) {
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
        return Err(AppError::not_found(format!(
            "no inbox item `{provider}:{key}` in this workspace"
        )));
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

    let open = ticket_repo::list_inbox(&state.db, workspace_id, Some("open"), Some(16))
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
        "gitea" => {
            let (full, number) = item.external_key.split_once('#')?;
            let (owner, repo) = full.split_once('/')?;
            (
                "get_issue",
                json!({ "owner": owner, "repo": repo, "number": number }),
            )
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
                    "summary": t.summary,
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
    state: Option<String>,
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<InboxQuery>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let filter = q.state.as_deref().map(str::trim).filter(|s| !s.is_empty());
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
    let llm = crate::chat_tools::composer_or_fallback_for(state, workspace_id, Some(user_id)).await;
    crate::chat_tools::require_taking_runner(&llm)?;
    let prep = prepare_finish(state, workspace_id, user_id, args).await?;
    let claimed = ticket_repo::claim_launch(
        &state.db,
        workspace_id,
        &prep.item.provider,
        &prep.item.external_key,
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    if claimed.is_none() {
        return Err(AppError::conflict("ticket already launched"));
    }
    state
        .orch
        .store
        .ensure_project(workspace_id, "workspace fleet", 3)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let ticket_kind = matches!(
        inbox_sources::kind_for(&prep.item.provider),
        InboxKind::Ticket
    );
    if ticket_kind {
        let _ = state
            .orch
            .store
            .set_auto_sync_external(workspace_id, true)
            .await;
    }
    let mut plan = build_multi_task_plan(workspace_id, None, prep.tasks);
    plan.source = "inbox".into();
    plan.policy.auto_sync_external = ticket_kind;
    crate::chat_tools::stamp_plan_llm(&mut plan, &llm);
    plan.policy.requested_by = Some(user_id);
    let (run, _) = match state
        .orch
        .store
        .insert_accepted_plan(workspace_id, plan)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            let _ = ticket_repo::reopen_ticket(
                &state.db,
                workspace_id,
                &prep.item.provider,
                &prep.item.external_key,
            )
            .await;
            return Err(AppError::bad_request(e.to_string()));
        }
    };
    let attached = ticket_repo::attach_launch_run(
        &state.db,
        workspace_id,
        &prep.item.provider,
        &prep.item.external_key,
        run.id,
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    if attached.is_none() {
        let _ = state.orch.store.request_cancel(workspace_id, run.id).await;
        let _ = ticket_repo::reopen_ticket(
            &state.db,
            workspace_id,
            &prep.item.provider,
            &prep.item.external_key,
        )
        .await;
        return Err(AppError::conflict("ticket already launched"));
    }
    let _ = crate::workforce::attach_run_to_goal(
        state,
        workspace_id,
        None,
        &prep.item.title,
        &prep.item.title,
        run.id,
        None,
    )
    .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linear_and_jira_keys() {
        // Unprefixed TEAM-N: resolve_ticket uses find_inbox_by_key, not a provider seed.
        let p = parse_ticket_ref("ENG-123").unwrap();
        assert_eq!(p.key, "ENG-123");
        assert!(p.provider.is_none());
    }

    #[test]
    fn parse_ticket_ref_linear_colon_prefix() {
        let p = parse_ticket_ref("linear:ENG-123").unwrap();
        assert_eq!(p.provider.as_deref(), Some("linear"));
        assert_eq!(p.key, "ENG-123");
    }

    #[test]
    fn parse_ticket_ref_blank_or_provider_without_key_is_none() {
        for raw in ["", "  ", "jira:"] {
            assert!(parse_ticket_ref(raw).is_none(), "expected None for {raw:?}");
        }
    }

    #[test]
    fn parse_ticket_ref_unknown_prefix_keeps_full_key() {
        let p = parse_ticket_ref("foo:BAR-1").unwrap();
        assert!(p.provider.is_none());
        assert_eq!(p.key, "foo:BAR-1");
    }

    #[test]
    fn parses_github_hash_and_url() {
        // Pasted owner/repo#N: resolve_ticket infers provider=github and seeds find_inbox.
        let p = parse_ticket_ref("acme/app#42").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#42");
    }

    #[test]
    fn parse_ticket_ref_hex_suffix_is_not_github_issue_5() {
        let p = parse_ticket_ref("live-sim/stack#5cc319fa").unwrap();
        assert!(p.provider.is_none(), "{p:?}");
        assert_eq!(p.key, "live-sim/stack#5cc319fa");
        assert!(github_key_from_url_or_slash("live-sim/stack#5cc319fa").is_none());
    }

    #[test]
    fn parse_ticket_ref_github_issues_url() {
        let p = parse_ticket_ref("https://github.com/acme/app/issues/7").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#7");
    }

    #[test]
    fn parse_github_owner_repo_num_splits_key() {
        let (o, r, n) = parse_github_owner_repo_num("acme/app#7").unwrap();
        assert_eq!((o, r, n), ("acme".into(), "app".into(), 7));
    }

    #[test]
    fn github_colon_prefix_normalizes_hash_and_url() {
        let p = parse_ticket_ref("github:acme/app#12").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#12");
        let p = parse_ticket_ref("github:https://github.com/acme/app/issues/7").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#7");
    }

    #[test]
    fn github_pull_and_http_urls_normalize_like_issues() {
        let p = parse_ticket_ref("http://github.com/acme/app/pull/9").unwrap();
        assert_eq!(p.provider.as_deref(), Some("github"));
        assert_eq!(p.key, "acme/app#9");
    }

    #[test]
    fn parse_github_owner_repo_num_rejects_empty_owner_or_repo() {
        for raw in ["/app#1", "acme/#1"] {
            assert!(
                parse_github_owner_repo_num(raw).is_none(),
                "expected None for {raw:?}"
            );
        }
    }

    #[test]
    fn inbox_finish_plan_is_work_then_review() {
        let ext = ExternalRef {
            provider: "jira".into(),
            key: "ENG-1".into(),
        };
        let tasks = inbox_plan_tasks("ENG-1: fix", "do the ticket", ext);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].plan_task_id, "work");
        assert_eq!(tasks[0].worker_profile, "coding");
        assert_eq!(tasks[1].plan_task_id, "review");
        assert_eq!(tasks[1].worker_profile, "reviewer");
        assert_eq!(tasks[1].depends_on, vec!["work".to_string()]);
    }

    #[test]
    fn inbox_slack_message_is_single_handle_ops() {
        let tasks = inbox_plan_tasks(
            "hello",
            "please look",
            ExternalRef {
                provider: "slack".into(),
                key: "C1:1".into(),
            },
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].plan_task_id, "handle");
        assert_eq!(tasks[0].worker_profile, "ops");
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn inbox_notion_page_is_single_handle_analysis() {
        let tasks = inbox_plan_tasks(
            "spec",
            "read this",
            ExternalRef {
                provider: "notion".into(),
                key: "page-1".into(),
            },
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].plan_task_id, "handle");
        assert_eq!(tasks[0].worker_profile, "analysis");
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn inbox_teams_message_is_single_handle_ops() {
        let tasks = inbox_plan_tasks(
            "hello",
            "please look",
            ExternalRef {
                provider: "teams".into(),
                key: "T1:1".into(),
            },
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].plan_task_id, "handle");
        assert_eq!(tasks[0].worker_profile, "ops");
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn inbox_confluence_page_is_single_handle_analysis() {
        let tasks = inbox_plan_tasks(
            "spec",
            "read this",
            ExternalRef {
                provider: "confluence".into(),
                key: "page-2".into(),
            },
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].plan_task_id, "handle");
        assert_eq!(tasks[0].worker_profile, "analysis");
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn unknown_inbox_provider_defaults_to_ticket_work_review() {
        let tasks = inbox_plan_tasks(
            "foo:1",
            "unknown provider",
            ExternalRef {
                provider: "foo".into(),
                key: "1".into(),
            },
        );
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].plan_task_id, "work");
        assert_eq!(tasks[0].worker_profile, "coding");
        assert_eq!(tasks[1].plan_task_id, "review");
        assert_eq!(tasks[1].worker_profile, "reviewer");
        assert_eq!(tasks[1].depends_on, vec!["work".to_string()]);
    }

    #[test]
    fn parses_slack_and_notion_keys() {
        // Extra-colon slack key: poll stores external_key as {channel}:{ts}.
        let p = parse_ticket_ref("slack:C123:171.1").unwrap();
        assert_eq!(p.provider.as_deref(), Some("slack"));
        assert_eq!(p.key, "C123:171.1");
    }

    #[test]
    fn parse_ticket_ref_slack_prefix_folds_case_and_trims_key() {
        let p = parse_ticket_ref("Slack: C123:171.1").unwrap();
        assert_eq!(p.provider.as_deref(), Some("slack"));
        assert_eq!(p.key, "C123:171.1");
    }

    #[test]
    fn parse_ticket_ref_teams_colon_prefix_keeps_extra_colon_key() {
        let p = parse_ticket_ref("teams:19:msg-1").unwrap();
        assert_eq!(p.provider.as_deref(), Some("teams"));
        assert_eq!(p.key, "19:msg-1");
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

    fn sample_inbox_ticket(workspace_id: Uuid) -> TicketInboxItem {
        let now = Utc::now();
        TicketInboxItem {
            id: Uuid::new_v4(),
            workspace_id,
            provider: "github".into(),
            external_key: "acme/app#1".into(),
            title: "secret from A".into(),
            url: None,
            state: "open".into(),
            summary: String::new(),
            run_id: None,
            last_seen_at: now,
            created_at: now,
        }
    }

    fn assert_inbox_cross_ws_denied(err: AppError) {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        let status = err.into_response().status();
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
            "finish/monitor of another workspace's ticket must be 403/404, got {status}"
        );
    }

    /// Knowing a ticket id from workspace A must not finish or monitor it via
    /// workspace B. Denied before launch; the A row is not mutated.
    #[test]
    fn finish_or_monitor_ticket_in_another_workspace_is_403() {
        let ws_a = Uuid::new_v4();
        let ws_b = Uuid::new_v4();
        let item = sample_inbox_ticket(ws_a);
        let ticket_id = item.id;
        assert_ne!(ws_a, ws_b);
        assert!(item.run_id.is_none());

        // Finish: route B + inbox row from A.
        let finish_err =
            require_ticket_workspace(&item, ws_b).expect_err("ticket from A must not finish via B");
        assert_inbox_cross_ws_denied(finish_err);
        assert!(
            require_ticket_workspace(&item, ws_a).is_ok(),
            "same-workspace finish must still be allowed"
        );

        // Monitor: not a member of A's workspace (B's session on A's route).
        let monitor_err =
            require_workspace_role(None).expect_err("monitor on another workspace must 403");
        assert_inbox_cross_ws_denied(monitor_err);
        assert!(
            require_workspace_role(Some("member".into())).is_ok(),
            "member of the route workspace may monitor that workspace"
        );

        // Exposed id is not a cross-workspace handle: parse treats it as a key
        // with no provider, so resolve stays scoped to the route workspace.
        let parsed = parse_ticket_ref(&ticket_id.to_string()).unwrap();
        assert!(
            parsed.provider.is_none(),
            "inbox row id must not imply a provider for finish via B"
        );
        assert_eq!(parsed.key, ticket_id.to_string());

        // Denied path must not look like a launched finish.
        assert!(item.run_id.is_none());
        assert_eq!(item.workspace_id, ws_a);
        assert_eq!(item.state, "open");
    }
}
