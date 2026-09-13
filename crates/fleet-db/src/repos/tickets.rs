//! Ticket monitor + inbox.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{TicketInboxItem, TicketMonitor};
use crate::pool::map_query_err;

const MONITOR_COLS: &str = r#"
    workspace_id, enabled, providers, github_repos, jira_jql,
    slack_channels, notion_databases, auto_launch, follow_connected,
    filters, launch_limit, poll_limit, hook_token, started_by,
    last_poll_at, last_error, created_at, updated_at
"#;

const INBOX_COLS: &str = r#"
    id, workspace_id, provider, external_key, title, url, state, summary,
    run_id, last_seen_at, created_at
"#;

#[derive(Debug, Clone)]
pub struct UpsertMonitor {
    pub workspace_id: Uuid,
    pub enabled: bool,
    pub providers: Vec<String>,
    pub github_repos: Vec<String>,
    pub jira_jql: Option<String>,
    pub slack_channels: Vec<String>,
    pub notion_databases: Vec<String>,
    pub auto_launch: bool,
    pub follow_connected: bool,
    pub filters: serde_json::Value,
    pub launch_limit: i32,
    pub poll_limit: i32,
    pub hook_token: String,
    pub started_by: Option<Uuid>,
}

pub async fn get_monitor(pool: &PgPool, workspace_id: Uuid) -> DbResult<Option<TicketMonitor>> {
    let q = format!(
        r#"
        SELECT {MONITOR_COLS}
        FROM ticket_monitors
        WHERE workspace_id = $1
        "#
    );
    sqlx::query_as::<_, TicketMonitor>(&q)
        .bind(workspace_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_enabled_workspace_ids(pool: &PgPool) -> DbResult<Vec<Uuid>> {
    let rows = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT workspace_id
        FROM ticket_monitors
        WHERE enabled
        ORDER BY workspace_id
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)?;
    Ok(rows)
}

pub async fn get_monitor_by_hook_token(
    pool: &PgPool,
    hook_token: &str,
) -> DbResult<Option<TicketMonitor>> {
    let token = hook_token.trim();
    if token.is_empty() {
        return Ok(None);
    }
    let q = format!(
        r#"
        SELECT {MONITOR_COLS}
        FROM ticket_monitors
        WHERE hook_token = $1
        "#
    );
    sqlx::query_as::<_, TicketMonitor>(&q)
        .bind(token)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn upsert_monitor(pool: &PgPool, spec: &UpsertMonitor) -> DbResult<TicketMonitor> {
    let q = format!(
        r#"
        INSERT INTO ticket_monitors (
            workspace_id, enabled, providers, github_repos, jira_jql,
            slack_channels, notion_databases, auto_launch, follow_connected,
            filters, launch_limit, poll_limit, hook_token, started_by,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, now())
        ON CONFLICT (workspace_id) DO UPDATE SET
            enabled = EXCLUDED.enabled,
            providers = EXCLUDED.providers,
            github_repos = EXCLUDED.github_repos,
            jira_jql = EXCLUDED.jira_jql,
            slack_channels = EXCLUDED.slack_channels,
            notion_databases = EXCLUDED.notion_databases,
            auto_launch = EXCLUDED.auto_launch,
            follow_connected = EXCLUDED.follow_connected,
            filters = EXCLUDED.filters,
            launch_limit = EXCLUDED.launch_limit,
            poll_limit = EXCLUDED.poll_limit,
            hook_token = COALESCE(ticket_monitors.hook_token, EXCLUDED.hook_token),
            started_by = COALESCE(EXCLUDED.started_by, ticket_monitors.started_by),
            updated_at = now()
        RETURNING {MONITOR_COLS}
        "#
    );
    sqlx::query_as::<_, TicketMonitor>(&q)
        .bind(spec.workspace_id)
        .bind(spec.enabled)
        .bind(&spec.providers)
        .bind(&spec.github_repos)
        .bind(&spec.jira_jql)
        .bind(&spec.slack_channels)
        .bind(&spec.notion_databases)
        .bind(spec.auto_launch)
        .bind(spec.follow_connected)
        .bind(&spec.filters)
        .bind(spec.launch_limit)
        .bind(spec.poll_limit)
        .bind(&spec.hook_token)
        .bind(spec.started_by)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn mark_polled(
    pool: &PgPool,
    workspace_id: Uuid,
    at: DateTime<Utc>,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query(
        r#"
        UPDATE ticket_monitors
        SET last_poll_at = $2, last_error = $3, updated_at = now()
        WHERE workspace_id = $1
        "#,
    )
    .bind(workspace_id)
    .bind(at)
    .bind(error)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct UpsertInbox {
    pub workspace_id: Uuid,
    pub provider: String,
    pub external_key: String,
    pub title: String,
    pub url: Option<String>,
    pub summary: String,
    pub provider_closed: bool,
}

pub async fn upsert_inbox(pool: &PgPool, item: &UpsertInbox) -> DbResult<TicketInboxItem> {
    let next_state = if item.provider_closed { "done" } else { "open" };
    let q = format!(
        r#"
        INSERT INTO ticket_inbox (
            workspace_id, provider, external_key, title, url, state, summary, last_seen_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, now())
        ON CONFLICT (workspace_id, provider, external_key) DO UPDATE SET
            title = EXCLUDED.title,
            url = EXCLUDED.url,
            summary = CASE
                WHEN ticket_inbox.state IN ('launched', 'ignored') THEN ticket_inbox.summary
                ELSE EXCLUDED.summary
            END,
            state = CASE
                WHEN ticket_inbox.state = 'ignored' THEN 'ignored'
                WHEN EXCLUDED.state = 'done' THEN 'done'
                WHEN ticket_inbox.state = 'launched' THEN 'launched'
                ELSE ticket_inbox.state
            END,
            last_seen_at = now()
        RETURNING {INBOX_COLS}
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(item.workspace_id)
        .bind(&item.provider)
        .bind(&item.external_key)
        .bind(&item.title)
        .bind(&item.url)
        .bind(next_state)
        .bind(&item.summary)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_inbox(
    pool: &PgPool,
    workspace_id: Uuid,
    state: Option<&str>,
    limit: Option<i64>,
) -> DbResult<Vec<TicketInboxItem>> {
    let limit = limit.unwrap_or(200).clamp(1, 500);
    if let Some(state) = state.map(str::trim).filter(|s| !s.is_empty()) {
        let q = format!(
            r#"
            SELECT {INBOX_COLS}
            FROM ticket_inbox
            WHERE workspace_id = $1 AND state = $2
            ORDER BY last_seen_at DESC
            LIMIT $3
            "#
        );
        return sqlx::query_as::<_, TicketInboxItem>(&q)
            .bind(workspace_id)
            .bind(state)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_query_err);
    }
    let q = format!(
        r#"
        SELECT {INBOX_COLS}
        FROM ticket_inbox
        WHERE workspace_id = $1
        ORDER BY
            CASE state
                WHEN 'open' THEN 0
                WHEN 'launched' THEN 1
                WHEN 'ignored' THEN 2
                ELSE 3
            END,
            last_seen_at DESC
        LIMIT $2
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_inbox(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
) -> DbResult<Option<TicketInboxItem>> {
    let q = format!(
        r#"
        SELECT {INBOX_COLS}
        FROM ticket_inbox
        WHERE workspace_id = $1
          AND lower(provider) = lower($2)
          AND lower(external_key) = lower($3)
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(external_key)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_inbox_by_key(
    pool: &PgPool,
    workspace_id: Uuid,
    external_key: &str,
) -> DbResult<Vec<TicketInboxItem>> {
    let q = format!(
        r#"
        SELECT {INBOX_COLS}
        FROM ticket_inbox
        WHERE workspace_id = $1 AND lower(external_key) = lower($2)
        ORDER BY last_seen_at DESC
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(external_key)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

/// CAS an open row to `launched` before a plan exists so a second Finish
/// cannot insert another run. `run_id` is attached afterwards.
pub async fn claim_launch(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
) -> DbResult<Option<TicketInboxItem>> {
    let q = format!(
        r#"
        UPDATE ticket_inbox
        SET state = 'launched', last_seen_at = now()
        WHERE workspace_id = $1
          AND lower(provider) = lower($2)
          AND lower(external_key) = lower($3)
          AND state = 'open'
          AND run_id IS NULL
        RETURNING {INBOX_COLS}
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(external_key)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn attach_launch_run(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
    run_id: Uuid,
) -> DbResult<Option<TicketInboxItem>> {
    let q = format!(
        r#"
        UPDATE ticket_inbox
        SET run_id = $4
        WHERE workspace_id = $1
          AND lower(provider) = lower($2)
          AND lower(external_key) = lower($3)
          AND state = 'launched'
          AND run_id IS NULL
        RETURNING {INBOX_COLS}
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(external_key)
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn mark_launched(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
    run_id: Uuid,
) -> DbResult<Option<TicketInboxItem>> {
    if claim_launch(pool, workspace_id, provider, external_key)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    attach_launch_run(pool, workspace_id, provider, external_key, run_id).await
}

/// Run died or was cancelled — ticket goes back to open so the next poll works it again.
pub async fn reopen_ticket(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
) -> DbResult<Option<TicketInboxItem>> {
    let q = format!(
        r#"
        UPDATE ticket_inbox
        SET state = 'open', run_id = NULL
        WHERE workspace_id = $1
          AND lower(provider) = lower($2)
          AND lower(external_key) = lower($3)
          AND state IN ('launched', 'done')
        RETURNING {INBOX_COLS}
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(external_key)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Cancel/fail of a run — every inbox row still `launched` on that run reopens.
pub async fn reopen_tickets_for_run(
    pool: &PgPool,
    workspace_id: Uuid,
    run_id: Uuid,
) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE ticket_inbox
        SET state = 'open', run_id = NULL, last_seen_at = now()
        WHERE workspace_id = $1
          AND run_id = $2
          AND state = 'launched'
        "#,
    )
    .bind(workspace_id)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

pub async fn reopen_tickets_for_run_id(pool: &PgPool, run_id: Uuid) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE ticket_inbox
        SET state = 'open', run_id = NULL, last_seen_at = now()
        WHERE run_id = $1
          AND state = 'launched'
        "#,
    )
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

pub async fn mark_inbox_done(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    external_key: &str,
) -> DbResult<Option<TicketInboxItem>> {
    let q = format!(
        r#"
        UPDATE ticket_inbox
        SET state = 'done'
        WHERE workspace_id = $1
          AND lower(provider) = lower($2)
          AND lower(external_key) = lower($3)
        RETURNING {INBOX_COLS}
        "#
    );
    sqlx::query_as::<_, TicketInboxItem>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(external_key)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}
