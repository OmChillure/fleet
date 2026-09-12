//! Workspace LLM/API keys for in-container guests. Ciphertext is opaque JSON.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::pool::map_query_err;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceLlmKey {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub ciphertext: Value,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceLlmKeyPublic {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub revoked: bool,
}

impl WorkspaceLlmKey {
    pub fn as_public(&self) -> WorkspaceLlmKeyPublic {
        WorkspaceLlmKeyPublic {
            id: self.id,
            workspace_id: self.workspace_id,
            provider: self.provider.clone(),
            created_at: self.created_at,
            revoked: self.revoked_at.is_some(),
        }
    }
}

pub async fn insert(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    ciphertext: Value,
    created_by: Option<Uuid>,
) -> DbResult<WorkspaceLlmKey> {
    sqlx::query_as::<_, WorkspaceLlmKey>(
        r#"
        INSERT INTO workspace_llm_keys (id, workspace_id, provider, ciphertext, created_by)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, workspace_id, provider, ciphertext, created_by, created_at, revoked_at
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider.trim().to_ascii_lowercase())
    .bind(ciphertext)
    .bind(created_by)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

pub async fn list_active(pool: &PgPool, workspace_id: Uuid) -> DbResult<Vec<WorkspaceLlmKey>> {
    sqlx::query_as::<_, WorkspaceLlmKey>(
        r#"
        SELECT id, workspace_id, provider, ciphertext, created_by, created_at, revoked_at
        FROM workspace_llm_keys
        WHERE workspace_id = $1 AND revoked_at IS NULL
        ORDER BY created_at DESC
        "#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn has_active(pool: &PgPool, workspace_id: Uuid, provider: &str) -> DbResult<bool> {
    let n = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::bigint
        FROM workspace_llm_keys
        WHERE workspace_id = $1
          AND provider = $2
          AND revoked_at IS NULL
        "#,
    )
    .bind(workspace_id)
    .bind(provider.trim().to_ascii_lowercase())
    .fetch_one(pool)
    .await
    .map_err(map_query_err)?;
    Ok(n > 0)
}

pub async fn active_ciphertext(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<Option<Value>> {
    sqlx::query_scalar::<_, Value>(
        r#"
        SELECT ciphertext
        FROM workspace_llm_keys
        WHERE workspace_id = $1
          AND provider = $2
          AND revoked_at IS NULL
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(workspace_id)
    .bind(provider.trim().to_ascii_lowercase())
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn revoke(pool: &PgPool, workspace_id: Uuid, id: Uuid) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE workspace_llm_keys
        SET revoked_at = now()
        WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL
        "#,
    )
    .bind(id)
    .bind(workspace_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}
