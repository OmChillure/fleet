use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbResult;
use crate::pool::map_query_err;

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
