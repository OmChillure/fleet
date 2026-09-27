use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{
    Workspace, WorkspaceInvite, WorkspaceMemberPublic, WorkspaceWithRole, parse_egress_mode,
    parse_min_isolation, parse_worker_host_run_mode, parse_worker_host_send_policy,
};
use crate::pool::map_query_err;

const WORKSPACE_COLS: &str = r#"
    id, slug, name, created_at, updated_at, version
"#;

/// Oldest workspace (kind/compose cluster-host bootstrap).
pub async fn first_id(pool: &PgPool) -> DbResult<Option<Uuid>> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM workspaces ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

#[derive(Debug, Clone)]
pub struct NewWorkspace {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn insert_tx(tx: &mut Transaction<'_, Postgres>, new: &NewWorkspace) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $4)
        "#,
    )
    .bind(new.id)
    .bind(&new.slug)
    .bind(&new.name)
    .bind(new.created_at)
    .execute(&mut **tx)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn add_member_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    user_id: Uuid,
    role: &str,
    created_at: DateTime<Utc>,
) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO workspace_members (workspace_id, user_id, role, created_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .bind(created_at)
    .execute(&mut **tx)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

/// Create personal workspace + owner membership in one transaction step set.
pub async fn create_personal_workspace_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    user_id: Uuid,
    slug: &str,
    name: &str,
    at: DateTime<Utc>,
) -> DbResult<()> {
    insert_tx(
        tx,
        &NewWorkspace {
            id: workspace_id,
            slug: slug.to_string(),
            name: name.to_string(),
            created_at: at,
        },
    )
    .await?;
    add_member_tx(tx, workspace_id, user_id, "owner", at).await?;
    Ok(())
}

/// Workspaces the user belongs to, oldest first. Login default is the
/// oldest *owned* workspace (`ensure_default_workspace`), not this first row.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> DbResult<Vec<WorkspaceWithRole>> {
    sqlx::query_as::<_, WorkspaceWithRole>(
        r#"
        SELECT w.id, w.slug, w.name, w.created_at, w.updated_at, w.version, m.role
        FROM workspaces w
        INNER JOIN workspace_members m ON m.workspace_id = w.id
        WHERE m.user_id = $1
        ORDER BY w.created_at ASC, w.id ASC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn find_for_user(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> DbResult<Option<WorkspaceWithRole>> {
    sqlx::query_as::<_, WorkspaceWithRole>(
        r#"
        SELECT w.id, w.slug, w.name, w.created_at, w.updated_at, w.version, m.role
        FROM workspaces w
        INNER JOIN workspace_members m ON m.workspace_id = w.id
        WHERE w.id = $1 AND m.user_id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

/// Create a workspace and make `user_id` the owner (standalone, not in a parent tx).
pub async fn create_owned(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    slug: &str,
    name: &str,
    at: DateTime<Utc>,
) -> DbResult<Workspace> {
    let mut tx = pool.begin().await.map_err(DbError::from)?;
    create_personal_workspace_tx(&mut tx, workspace_id, user_id, slug, name, at).await?;
    let q = format!("SELECT {WORKSPACE_COLS} FROM workspaces WHERE id = $1");
    let ws = sqlx::query_as::<_, Workspace>(&q)
        .bind(workspace_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_query_err)?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(ws)
}

/// Role for user in workspace, if a member.
pub async fn user_role(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> DbResult<Option<String>> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT role FROM workspace_members
        WHERE workspace_id = $1 AND user_id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    Ok(row)
}

/// Timestamp of the partners disclaimer ack, or `None` if not accepted.
///
/// Missing workspace → [`DbError::NotFound`]. Default (NULL) is not accepted.
pub async fn partners_disclaimer_accepted_at(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Option<DateTime<Utc>>> {
    let row = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        r#"
        SELECT partners_disclaimer_accepted_at
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

/// Workspace minimum isolation (`process` | `container` | `vm`).
///
/// Missing workspace → [`DbError::NotFound`]. New rows default to `process`.
pub async fn min_isolation(pool: &PgPool, workspace_id: Uuid) -> DbResult<String> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT min_isolation
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

/// Set workspace minimum isolation. Rejects unknown strings in-process.
pub async fn set_min_isolation(
    pool: &PgPool,
    workspace_id: Uuid,
    isolation: &str,
) -> DbResult<String> {
    let isolation = parse_min_isolation(isolation).ok_or_else(|| {
        DbError::Conflict("min_isolation must be process, container, or vm".into())
    })?;
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE workspaces
        SET min_isolation = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING min_isolation
        "#,
    )
    .bind(workspace_id)
    .bind(isolation)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

/// Ceiling for new pairs: `owner_only` | `allowlist` | `workspace`.
pub async fn default_send_policy(pool: &PgPool, workspace_id: Uuid) -> DbResult<String> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT default_send_policy
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn set_default_send_policy(
    pool: &PgPool,
    workspace_id: Uuid,
    policy: &str,
) -> DbResult<String> {
    let policy = parse_worker_host_send_policy(policy).ok_or_else(|| {
        DbError::Conflict("default_send_policy must be owner_only, allowlist, or workspace".into())
    })?;
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE workspaces
        SET default_send_policy = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING default_send_policy
        "#,
    )
    .bind(workspace_id)
    .bind(policy)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn is_member(pool: &PgPool, workspace_id: Uuid, user_id: Uuid) -> DbResult<bool> {
    Ok(user_role(pool, workspace_id, user_id).await?.is_some())
}

pub async fn default_run_mode(pool: &PgPool, workspace_id: Uuid) -> DbResult<String> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT default_run_mode
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn set_default_run_mode(
    pool: &PgPool,
    workspace_id: Uuid,
    mode: &str,
) -> DbResult<String> {
    let mode = parse_worker_host_run_mode(mode).ok_or_else(|| {
        DbError::Conflict("default_run_mode must be on_pc or in_container".into())
    })?;
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE workspaces
        SET default_run_mode = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING default_run_mode
        "#,
    )
    .bind(workspace_id)
    .bind(mode)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub const EGRESS_ALLOWLIST_MAX: usize = 2000;

pub async fn default_egress(pool: &PgPool, workspace_id: Uuid) -> DbResult<String> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT default_egress
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn default_egress_allowlist(pool: &PgPool, workspace_id: Uuid) -> DbResult<String> {
    let row = sqlx::query_scalar::<_, String>(
        r#"
        SELECT default_egress_allowlist
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn set_default_egress(pool: &PgPool, workspace_id: Uuid, mode: &str) -> DbResult<String> {
    let mode = parse_egress_mode(mode).ok_or_else(|| {
        DbError::Conflict("default_egress must be inherit, none, allowlist, or open".into())
    })?;
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE workspaces
        SET default_egress = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING default_egress
        "#,
    )
    .bind(workspace_id)
    .bind(mode)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn set_default_egress_allowlist(
    pool: &PgPool,
    workspace_id: Uuid,
    raw: &str,
) -> DbResult<String> {
    let list = raw.trim();
    if list.len() > EGRESS_ALLOWLIST_MAX {
        return Err(DbError::Conflict(format!(
            "default_egress_allowlist must be at most {EGRESS_ALLOWLIST_MAX} characters"
        )));
    }
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE workspaces
        SET default_egress_allowlist = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING default_egress_allowlist
        "#,
    )
    .bind(workspace_id)
    .bind(list)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub const CHAT_MAX_TURNS_DEFAULT: i32 = 32;
pub const CHAT_MAX_TURNS_MIN: i32 = 1;
pub const CHAT_MAX_TURNS_MAX: i32 = 128;

pub fn parse_chat_max_turns(n: i32) -> Option<i32> {
    (CHAT_MAX_TURNS_MIN..=CHAT_MAX_TURNS_MAX)
        .contains(&n)
        .then_some(n)
}

pub async fn chat_max_turns(pool: &PgPool, workspace_id: Uuid) -> DbResult<i32> {
    let row = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT chat_max_turns
        FROM workspaces
        WHERE id = $1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub async fn set_chat_max_turns(pool: &PgPool, workspace_id: Uuid, turns: i32) -> DbResult<i32> {
    let turns = parse_chat_max_turns(turns).ok_or_else(|| {
        DbError::Conflict(format!(
            "chat_max_turns must be {CHAT_MAX_TURNS_MIN}–{CHAT_MAX_TURNS_MAX}"
        ))
    })?;
    let stored = sqlx::query_scalar::<_, i32>(
        r#"
        UPDATE workspaces
        SET chat_max_turns = $2,
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING chat_max_turns
        "#,
    )
    .bind(workspace_id)
    .bind(turns)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    stored.ok_or_else(|| DbError::NotFound("workspace".into()))
}

/// Record the partners disclaimer ack. First write wins.
///
/// Does not enable lending, borrowing, enqueue, or assignable runners.
pub async fn accept_partners_disclaimer(
    pool: &PgPool,
    workspace_id: Uuid,
    at: DateTime<Utc>,
) -> DbResult<DateTime<Utc>> {
    let accepted_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        r#"
        UPDATE workspaces
        SET partners_disclaimer_accepted_at = COALESCE(partners_disclaimer_accepted_at, $2),
            updated_at = now(),
            version = version + 1
        WHERE id = $1
        RETURNING partners_disclaimer_accepted_at
        "#,
    )
    .bind(workspace_id)
    .bind(at)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    accepted_at.ok_or_else(|| DbError::NotFound("workspace".into()))
}

pub fn normalize_member_role(role: &str) -> Option<&'static str> {
    match role.trim().to_ascii_lowercase().as_str() {
        "owner" => Some("owner"),
        "admin" => Some("admin"),
        "member" => Some("member"),
        _ => None,
    }
}

pub fn normalize_invite_role(role: &str) -> Option<&'static str> {
    match role.trim().to_ascii_lowercase().as_str() {
        "admin" => Some("admin"),
        "member" => Some("member"),
        _ => None,
    }
}

pub async fn list_members(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<WorkspaceMemberPublic>> {
    sqlx::query_as::<_, WorkspaceMemberPublic>(
        r#"
        SELECT m.workspace_id, m.user_id, m.role, m.created_at,
               u.email, u.display_name
        FROM workspace_members m
        INNER JOIN users u ON u.id = m.user_id
        WHERE m.workspace_id = $1
        ORDER BY m.created_at ASC, m.user_id ASC
        "#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn count_owners(pool: &PgPool, workspace_id: Uuid) -> DbResult<i64> {
    let n = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::bigint FROM workspace_members
        WHERE workspace_id = $1 AND role = 'owner'
        "#,
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)?;
    Ok(n)
}

pub async fn add_member(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    role: &str,
    created_at: DateTime<Utc>,
) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO workspace_members (workspace_id, user_id, role, created_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (workspace_id, user_id) DO NOTHING
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .bind(created_at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn set_member_role(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE workspace_members SET role = $3
        WHERE workspace_id = $1 AND user_id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

pub async fn remove_member(pool: &PgPool, workspace_id: Uuid, user_id: Uuid) -> DbResult<bool> {
    let mut tx = pool.begin().await.map_err(DbError::from)?;
    let res = sqlx::query(
        r#"
        DELETE FROM workspace_members
        WHERE workspace_id = $1 AND user_id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;
    if res.rows_affected() == 1 {
        sqlx::query(
            r#"
            DELETE FROM worker_host_send_allowlist a
            USING worker_hosts h
            WHERE a.host_id = h.id
              AND h.workspace_id = $1
              AND a.user_id = $2
            "#,
        )
        .bind(workspace_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(map_query_err)?;
    }
    tx.commit().await.map_err(DbError::from)?;
    Ok(res.rows_affected() == 1)
}

const INVITE_COLS: &str = r#"
    id, workspace_id, email_normalized, role, token, invited_by,
    created_at, expires_at, accepted_at, accepted_user_id, revoked_at
"#;

pub async fn insert_invite(pool: &PgPool, invite: &WorkspaceInvite) -> DbResult<WorkspaceInvite> {
    // Expired open rows still occupy the partial unique index (it cannot
    // use now()). Clear them so a re-invite after expiry can succeed.
    sqlx::query(
        r#"
        UPDATE workspace_invites
        SET revoked_at = $3
        WHERE workspace_id = $1
          AND email_normalized = $2
          AND accepted_at IS NULL AND revoked_at IS NULL
          AND expires_at <= $3
        "#,
    )
    .bind(invite.workspace_id)
    .bind(&invite.email_normalized)
    .bind(invite.created_at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;

    let q = format!(
        r#"
        INSERT INTO workspace_invites (
            id, workspace_id, email_normalized, role, token, invited_by,
            created_at, expires_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING {INVITE_COLS}
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(invite.id)
        .bind(invite.workspace_id)
        .bind(&invite.email_normalized)
        .bind(&invite.role)
        .bind(&invite.token)
        .bind(invite.invited_by)
        .bind(invite.created_at)
        .bind(invite.expires_at)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_open_invites(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<WorkspaceInvite>> {
    let q = format!(
        r#"
        SELECT {INVITE_COLS} FROM workspace_invites
        WHERE workspace_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL
          AND expires_at > now()
        ORDER BY created_at DESC
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_invite_by_token(
    pool: &PgPool,
    token: &str,
) -> DbResult<Option<WorkspaceInvite>> {
    let q = format!(
        r#"
        SELECT {INVITE_COLS} FROM workspace_invites
        WHERE token = $1
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(token)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_open_invite_by_token(
    pool: &PgPool,
    token: &str,
    now: DateTime<Utc>,
) -> DbResult<Option<WorkspaceInvite>> {
    let q = format!(
        r#"
        SELECT {INVITE_COLS} FROM workspace_invites
        WHERE token = $1
          AND accepted_at IS NULL AND revoked_at IS NULL
          AND expires_at > $2
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(token)
        .bind(now)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get_invite(
    pool: &PgPool,
    workspace_id: Uuid,
    invite_id: Uuid,
) -> DbResult<Option<WorkspaceInvite>> {
    let q = format!(
        r#"
        SELECT {INVITE_COLS} FROM workspace_invites
        WHERE workspace_id = $1 AND id = $2
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(workspace_id)
        .bind(invite_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn revoke_invite(
    pool: &PgPool,
    workspace_id: Uuid,
    invite_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE workspace_invites
        SET revoked_at = $3
        WHERE workspace_id = $1 AND id = $2
          AND accepted_at IS NULL AND revoked_at IS NULL
        "#,
    )
    .bind(workspace_id)
    .bind(invite_id)
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// Pending invites for this email that have not expired.
pub async fn list_pending_invites_for_email(
    pool: &PgPool,
    email_normalized: &str,
    now: DateTime<Utc>,
) -> DbResult<Vec<WorkspaceInvite>> {
    let q = format!(
        r#"
        SELECT {INVITE_COLS} FROM workspace_invites
        WHERE email_normalized = $1
          AND accepted_at IS NULL AND revoked_at IS NULL
          AND expires_at > $2
        ORDER BY created_at ASC
        "#
    );
    sqlx::query_as::<_, WorkspaceInvite>(&q)
        .bind(email_normalized)
        .bind(now)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn accept_invite(
    pool: &PgPool,
    invite_id: Uuid,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE workspace_invites
        SET accepted_at = $3, accepted_user_id = $2
        WHERE id = $1 AND accepted_at IS NULL AND revoked_at IS NULL
          AND expires_at > $3
        "#,
    )
    .bind(invite_id)
    .bind(user_id)
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// Add memberships for every open invite matching this email (signup / login).
pub async fn accept_pending_invites_for_user(
    pool: &PgPool,
    user_id: Uuid,
    email_normalized: &str,
    now: DateTime<Utc>,
) -> DbResult<usize> {
    let invites = list_pending_invites_for_email(pool, email_normalized, now).await?;
    let mut n = 0usize;
    for inv in invites {
        add_member(pool, inv.workspace_id, user_id, &inv.role, now).await?;
        if accept_invite(pool, inv.id, user_id, now).await? {
            n += 1;
        }
    }
    Ok(n)
}
