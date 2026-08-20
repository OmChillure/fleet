use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{Workspace, WorkspaceWithRole};
use crate::pool::map_query_err;

const WORKSPACE_COLS: &str = r#"
    id, slug, name, created_at, updated_at, version
"#;

#[derive(Debug, Clone)]
pub struct NewWorkspace {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn insert_tx(
    tx: &mut Transaction<'_, Postgres>,
    new: &NewWorkspace,
) -> DbResult<()> {
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

/// Workspaces the user belongs to, oldest first (default = first).
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
