//! Workspace integration connection repository.
//!
//! Credentials are stored for control-plane use (AI / orchestrator proxy).
//! Callers that write secrets should pass **already encrypted** envelopes
//! (see `fleet_integrations::CredentialsKey`). List helpers never need to
//! return secret fields — use [`IntegrationConnectionPublic`] at the API boundary.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::IntegrationConnection;
use crate::pool::map_query_err;

const COLS: &str = r#"
    id, workspace_id, provider, status, account_label, config, credentials,
    connected_by, last_error, connected_at, updated_at, version
"#;

#[derive(Debug, Clone)]
pub struct UpsertConnection {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub account_label: Option<String>,
    pub config: Value,
    pub credentials: Value,
    pub connected_by: Option<Uuid>,
    pub at: DateTime<Utc>,
}

/// Active (connected) rows for a workspace — includes credentials for internal use.
pub async fn list_for_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<IntegrationConnection>> {
    let q = format!(
        r#"
        SELECT {COLS}
        FROM integration_connections
        WHERE workspace_id = $1 AND status = 'connected'
        ORDER BY provider ASC
        "#
    );
    sqlx::query_as::<_, IntegrationConnection>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

/// All rows including disconnected (for admin/debug). Prefer list_for_workspace.
pub async fn list_all_for_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<IntegrationConnection>> {
    let q = format!(
        r#"
        SELECT {COLS}
        FROM integration_connections
        WHERE workspace_id = $1
        ORDER BY provider ASC
        "#
    );
    sqlx::query_as::<_, IntegrationConnection>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_connected(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<Option<IntegrationConnection>> {
    let q = format!(
        r#"
        SELECT {COLS}
        FROM integration_connections
        WHERE workspace_id = $1 AND provider = $2 AND status = 'connected'
        "#
    );
    sqlx::query_as::<_, IntegrationConnection>(&q)
        .bind(workspace_id)
        .bind(provider)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Insert or re-connect a provider for a workspace (replaces credentials).
pub async fn upsert(pool: &PgPool, new: &UpsertConnection) -> DbResult<IntegrationConnection> {
    let q = format!(
        r#"
        INSERT INTO integration_connections (
            id, workspace_id, provider, status, account_label, config, credentials,
            connected_by, last_error, connected_at, updated_at, version
        )
        VALUES ($1, $2, $3, 'connected', $4, $5, $6, $7, NULL, $8, $8, 1)
        ON CONFLICT (workspace_id, provider)
        DO UPDATE SET
            status = 'connected',
            account_label = EXCLUDED.account_label,
            config = EXCLUDED.config,
            credentials = EXCLUDED.credentials,
            connected_by = EXCLUDED.connected_by,
            last_error = NULL,
            connected_at = EXCLUDED.connected_at,
            updated_at = EXCLUDED.updated_at,
            version = integration_connections.version + 1
        RETURNING {COLS}
        "#
    );
    sqlx::query_as::<_, IntegrationConnection>(&q)
        .bind(new.id)
        .bind(new.workspace_id)
        .bind(&new.provider)
        .bind(&new.account_label)
        .bind(&new.config)
        .bind(&new.credentials)
        .bind(new.connected_by)
        .bind(new.at)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

/// Soft-disconnect: clear secrets, mark disconnected.
pub async fn disconnect(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE integration_connections
        SET status = 'disconnected',
            credentials = '{}'::jsonb,
            last_error = NULL,
            updated_at = now(),
            version = version + 1
        WHERE workspace_id = $1 AND provider = $2 AND status = 'connected'
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .execute(pool)
    .await
    .map_err(map_query_err)?;

    Ok(res.rows_affected() > 0)
}

/// Load credentials for the orchestrator / AI proxy (internal).
pub async fn credentials_for(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<Option<Value>> {
    let row = find_connected(pool, workspace_id, provider).await?;
    Ok(row.map(|r| r.credentials))
}

/// Mark a connection as errored (e.g. token revoked when AI calls the API).
pub async fn mark_error(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    message: &str,
) -> DbResult<()> {
    let res = sqlx::query(
        r#"
        UPDATE integration_connections
        SET status = 'error',
            last_error = $3,
            updated_at = now(),
            version = version + 1
        WHERE workspace_id = $1 AND provider = $2 AND status = 'connected'
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .bind(message)
    .execute(pool)
    .await
    .map_err(map_query_err)?;

    if res.rows_affected() == 0 {
        return Err(DbError::NotFound(format!(
            "no connected integration {provider} for workspace"
        )));
    }
    Ok(())
}
