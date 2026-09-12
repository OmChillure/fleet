//! Instance-admin aggregates. Cross-workspace; callers must authorize.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::WorkerHost;
use crate::pool::map_query_err;
use crate::repos::worker_hosts::HOST_COLS;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub status: String,
    pub instance_admin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub workspace_count: i64,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub active_sessions: i64,
    pub runners_created: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct AdminWorkspaceRow {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub min_isolation: String,
    pub member_count: i64,
    pub runner_count: i64,
    pub online_runners: i64,
    pub connected_integrations: i64,
    pub inbox_monitor: bool,
    pub partners_disclaimer: bool,
    pub owners: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct AdminAuthSessionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ProviderCount {
    pub provider: String,
    pub status: String,
    pub count: i64,
}

pub async fn list_users(pool: &PgPool) -> DbResult<Vec<AdminUserRow>> {
    sqlx::query_as::<_, AdminUserRow>(
        r#"
        SELECT
            u.id,
            u.email,
            u.display_name,
            u.status,
            u.instance_admin,
            u.created_at,
            u.updated_at,
            (SELECT COUNT(*) FROM workspace_members m WHERE m.user_id = u.id)::bigint
                AS workspace_count,
            (SELECT MAX(s.last_seen_at) FROM auth_sessions s WHERE s.user_id = u.id)
                AS last_seen_at,
            (
                SELECT COUNT(*) FROM auth_sessions s
                WHERE s.user_id = u.id
                  AND s.revoked_at IS NULL
                  AND s.expires_at > now()
                  AND s.absolute_expires_at > now()
            )::bigint AS active_sessions,
            (SELECT COUNT(*) FROM worker_hosts h WHERE h.created_by = u.id)::bigint
                AS runners_created
        FROM users u
        ORDER BY u.created_at DESC, u.id ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn list_workspaces(pool: &PgPool) -> DbResult<Vec<AdminWorkspaceRow>> {
    sqlx::query_as::<_, AdminWorkspaceRow>(
        r#"
        SELECT
            w.id,
            w.slug,
            w.name,
            w.created_at,
            w.min_isolation,
            (SELECT COUNT(*) FROM workspace_members m WHERE m.workspace_id = w.id)::bigint
                AS member_count,
            (SELECT COUNT(*) FROM worker_hosts h WHERE h.workspace_id = w.id)::bigint
                AS runner_count,
            (
                SELECT COUNT(*) FROM worker_hosts h
                WHERE h.workspace_id = w.id AND h.status = 'online'
            )::bigint AS online_runners,
            (
                SELECT COUNT(*) FROM integration_connections c
                WHERE c.workspace_id = w.id AND c.status = 'connected'
            )::bigint AS connected_integrations,
            EXISTS(
                SELECT 1 FROM ticket_monitors t
                WHERE t.workspace_id = w.id AND t.enabled
            ) AS inbox_monitor,
            (w.partners_disclaimer_accepted_at IS NOT NULL) AS partners_disclaimer,
            (
                SELECT string_agg(u.email, ', ' ORDER BY u.email)
                FROM workspace_members m
                JOIN users u ON u.id = m.user_id
                WHERE m.workspace_id = w.id AND m.role = 'owner'
            ) AS owners
        FROM workspaces w
        ORDER BY w.created_at DESC, w.id ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn list_hosts(pool: &PgPool) -> DbResult<Vec<WorkerHost>> {
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        ORDER BY last_seen_at DESC NULLS LAST, created_at DESC, id ASC
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_active_sessions(pool: &PgPool) -> DbResult<Vec<AdminAuthSessionRow>> {
    sqlx::query_as::<_, AdminAuthSessionRow>(
        r#"
        SELECT
            s.id,
            s.user_id,
            u.email,
            s.user_agent,
            host(s.ip) AS ip,
            s.created_at,
            s.last_seen_at,
            s.expires_at
        FROM auth_sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.revoked_at IS NULL
          AND s.expires_at > now()
          AND s.absolute_expires_at > now()
        ORDER BY s.last_seen_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_users(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn count_workspaces(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspaces")
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn count_hosts(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM worker_hosts")
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn count_pending_invites(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM workspace_invites
        WHERE accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_group(pool: &PgPool, sql: &str) -> DbResult<Vec<StatusCount>> {
    sqlx::query_as::<_, StatusCount>(sql)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn count_work_claims(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM work_claims GROUP BY status",
    )
    .await
}

pub async fn count_cases(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM cases GROUP BY status",
    )
    .await
}

pub async fn count_goals(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM workforce_goals GROUP BY status",
    )
    .await
}

pub async fn count_approvals(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM policy_approvals GROUP BY status",
    )
    .await
}

pub async fn count_inbox(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT state AS status, COUNT(*)::bigint AS count FROM ticket_inbox GROUP BY state",
    )
    .await
}

pub async fn count_inbox_monitors_enabled(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ticket_monitors WHERE enabled")
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn count_unpublished_events(pool: &PgPool) -> DbResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM control_plane_events WHERE published_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_connections(pool: &PgPool) -> DbResult<Vec<ProviderCount>> {
    sqlx::query_as::<_, ProviderCount>(
        r#"
        SELECT provider, status, COUNT(*)::bigint AS count
        FROM integration_connections
        GROUP BY provider, status
        ORDER BY provider, status
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_oauth_apps(pool: &PgPool) -> DbResult<Vec<ProviderCount>> {
    sqlx::query_as::<_, ProviderCount>(
        r#"
        SELECT provider, 'configured' AS status, COUNT(*)::bigint AS count
        FROM integration_oauth_apps
        GROUP BY provider
        ORDER BY provider
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_orch_sessions(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM worker_sessions GROUP BY status",
    )
    .await
}

pub async fn count_orch_tasks(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM board_tasks GROUP BY status",
    )
    .await
}

pub async fn count_orch_runs(pool: &PgPool) -> DbResult<Vec<StatusCount>> {
    count_group(
        pool,
        "SELECT status, COUNT(*)::bigint AS count FROM work_runs GROUP BY status",
    )
    .await
}

pub async fn workspace_names(
    pool: &PgPool,
) -> DbResult<Vec<(Uuid, String, String)>> {
    sqlx::query_as::<_, (Uuid, String, String)>("SELECT id, slug, name FROM workspaces")
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}
