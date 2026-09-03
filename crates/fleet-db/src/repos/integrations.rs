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
use crate::models::{IntegrationConnection, IntegrationOAuthApp};
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

/// Merge agent defaults into `config` without touching credentials.
pub async fn patch_agent_defaults(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    runner_id: Option<&str>,
    llm_provider: Option<&str>,
    model: Option<&str>,
) -> DbResult<IntegrationConnection> {
    let q = format!(
        r#"
        UPDATE integration_connections
        SET config = (config
                - 'default_runner_id'
                - 'default_provider'
                - 'default_model')
            || jsonb_strip_nulls(jsonb_build_object(
                'default_runner_id', $3::text,
                'default_provider', $4::text,
                'default_model', $5::text
            )),
            updated_at = now(),
            version = version + 1
        WHERE workspace_id = $1 AND provider = $2 AND status = 'connected'
        RETURNING {COLS}
        "#
    );
    sqlx::query_as::<_, IntegrationConnection>(&q)
        .bind(workspace_id)
        .bind(provider)
        .bind(runner_id)
        .bind(llm_provider)
        .bind(model)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("integration not connected".into()))
}

/// Soft-disconnect: clear secrets, mark disconnected.
pub async fn disconnect(pool: &PgPool, workspace_id: Uuid, provider: &str) -> DbResult<bool> {
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

pub async fn list_oauth_apps(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<IntegrationOAuthApp>> {
    sqlx::query_as::<_, IntegrationOAuthApp>(
        r#"
        SELECT workspace_id, provider, client_id, client_secret, created_by, created_at, updated_at
        FROM integration_oauth_apps
        WHERE workspace_id = $1
        ORDER BY provider
        "#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn get_oauth_app(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<Option<IntegrationOAuthApp>> {
    sqlx::query_as::<_, IntegrationOAuthApp>(
        r#"
        SELECT workspace_id, provider, client_id, client_secret, created_by, created_at, updated_at
        FROM integration_oauth_apps
        WHERE workspace_id = $1 AND provider = $2
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn upsert_oauth_app(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    client_id: &str,
    client_secret: &Value,
    created_by: Option<Uuid>,
    at: DateTime<Utc>,
) -> DbResult<IntegrationOAuthApp> {
    sqlx::query_as::<_, IntegrationOAuthApp>(
        r#"
        INSERT INTO integration_oauth_apps (
            workspace_id, provider, client_id, client_secret, created_by, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        ON CONFLICT (workspace_id, provider)
        DO UPDATE SET
            client_id = EXCLUDED.client_id,
            client_secret = EXCLUDED.client_secret,
            updated_at = EXCLUDED.updated_at
        RETURNING workspace_id, provider, client_id, client_secret, created_by, created_at, updated_at
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .bind(client_id)
    .bind(client_secret)
    .bind(created_by)
    .bind(at)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

/// Persist an OAuth `state` nonce as consumed. Returns `true` if this call
/// won (first Connect). Returns `false` if the nonce was already used.
///
/// Insert is the consume — do this before exchanging the code or writing
/// credentials so a parallel replay cannot attach a second connection.
pub async fn consume_oauth_state_nonce(
    pool: &PgPool,
    nonce: &str,
    workspace_id: Uuid,
    provider: &str,
) -> DbResult<bool> {
    if nonce.trim().is_empty() {
        return Ok(false);
    }
    let res = sqlx::query(
        r#"
        INSERT INTO integration_oauth_state_nonces (nonce, workspace_id, provider)
        VALUES ($1, $2, $3)
        ON CONFLICT (nonce) DO NOTHING
        "#,
    )
    .bind(nonce)
    .bind(workspace_id)
    .bind(provider)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

#[cfg(test)]
mod patch_agent_defaults_sql_tests {
    fn production_src() -> &'static str {
        let src = include_str!("integrations.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    /// Defaults PATCH is UPDATE … status='connected'. It must not INSERT.
    #[test]
    fn patch_agent_defaults_sql_updates_connected_only_never_inserts() {
        let src = fn_src(production_src(), "pub async fn patch_agent_defaults(");
        assert!(
            src.contains("UPDATE integration_connections"),
            "patch_agent_defaults must UPDATE: {src}"
        );
        assert!(
            src.contains("WHERE workspace_id = $1 AND provider = $2 AND status = 'connected'"),
            "must target the connected row only: {src}"
        );
        assert!(
            src.contains("default_runner_id")
                && src.contains("default_provider")
                && src.contains("default_model"),
            "must write agent defaults into config: {src}"
        );
        assert!(
            !src.to_ascii_uppercase().contains("INSERT"),
            "must not INSERT a connections row: {src}"
        );
        assert!(
            !src.contains("ON CONFLICT"),
            "must not upsert a fake connected row: {src}"
        );
        assert!(
            src.contains("NotFound"),
            "zero rows must be NotFound (HTTP 404), not a silent insert: {src}"
        );
    }
}
