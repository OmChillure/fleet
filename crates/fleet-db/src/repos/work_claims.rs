//! Work claim queue — foundation for 1000 parallel agents.
//!
//! Poll path uses `FOR UPDATE SKIP LOCKED` so agents never block each other
//! on the same ready row (no deadlock chains on the claim hot path).

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{NewWorkClaim, WorkClaim};
use crate::pool::map_query_err;

const CLAIM_COLS: &str = r#"
    id, workspace_id, kind, payload, status, lease_owner, lease_token,
    lease_expires_at, attempts, max_attempts, available_at, last_error,
    created_at, updated_at, version
"#;

pub async fn enqueue(pool: &PgPool, new: &NewWorkClaim) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO work_claims (
            id, workspace_id, kind, payload, status,
            max_attempts, available_at, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, 'ready', $5, $6, now(), now())
        "#,
    )
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(&new.kind)
    .bind(&new.payload)
    .bind(new.max_attempts)
    .bind(new.available_at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

/// Atomically claim one ready work item for `agent_id`.
///
/// Uses `FOR UPDATE SKIP LOCKED` — concurrent claimers never wait on each
/// other's row locks.
pub async fn claim_next(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    lease_for: Duration,
) -> DbResult<Option<WorkClaim>> {
    let lease_token = Uuid::now_v7();
    let lease_expires = Utc::now() + lease_for;
    let q = format!(
        r#"
        WITH cte AS (
            SELECT id FROM work_claims
            WHERE status = 'ready'
              AND available_at <= now()
              AND workspace_id = $1
            ORDER BY available_at, created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE work_claims w
        SET status = 'leased',
            lease_owner = $2,
            lease_token = $3,
            lease_expires_at = $4,
            attempts = attempts + 1,
            version = version + 1,
            updated_at = now()
        FROM cte
        WHERE w.id = cte.id
        RETURNING {CLAIM_COLS}
        "#
    );

    sqlx::query_as::<_, WorkClaim>(&q)
        .bind(workspace_id)
        .bind(agent_id)
        .bind(lease_token)
        .bind(lease_expires)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Heartbeat / extend lease — only the holder with the matching token succeeds.
pub async fn heartbeat(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
    lease_for: Duration,
) -> DbResult<bool> {
    let lease_expires = Utc::now() + lease_for;
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET lease_expires_at = $3,
            updated_at = now()
        WHERE id = $1
          AND lease_token = $2
          AND status = 'leased'
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .bind(lease_expires)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

pub async fn complete(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET status = 'done',
            lease_owner = NULL,
            lease_token = NULL,
            lease_expires_at = NULL,
            version = version + 1,
            updated_at = now()
        WHERE id = $1
          AND lease_token = $2
          AND status = 'leased'
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

pub async fn fail(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
    error: &str,
    retry_after: Option<DateTime<Utc>>,
) -> DbResult<bool> {
    // If under max_attempts → ready again; else dead.
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET status = CASE
                WHEN attempts >= max_attempts THEN 'dead'
                ELSE 'ready'
            END,
            last_error = $3,
            available_at = COALESCE($4, now()),
            lease_owner = NULL,
            lease_token = NULL,
            lease_expires_at = NULL,
            version = version + 1,
            updated_at = now()
        WHERE id = $1
          AND lease_token = $2
          AND status = 'leased'
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .bind(error)
    .bind(retry_after)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// Reclaim expired leases (background sweeper). Safe under concurrency:
/// each row update is independent; no multi-row lock order.
pub async fn reclaim_expired_leases(pool: &PgPool, limit: i64) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET status = 'ready',
            lease_owner = NULL,
            lease_token = NULL,
            lease_expires_at = NULL,
            available_at = now(),
            version = version + 1,
            updated_at = now()
        WHERE id IN (
            SELECT id FROM work_claims
            WHERE status = 'leased'
              AND lease_expires_at IS NOT NULL
              AND lease_expires_at < now()
            ORDER BY lease_expires_at
            FOR UPDATE SKIP LOCKED
            LIMIT $1
        )
        "#,
    )
    .bind(limit)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}
