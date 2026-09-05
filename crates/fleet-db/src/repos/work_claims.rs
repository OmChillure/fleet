//! Work claim queue — foundation for 1000 parallel agents.
//!
//! Poll path uses `FOR UPDATE SKIP LOCKED` so agents never block each other
//! on the same ready row (no deadlock chains on the claim hot path).

use chrono::{DateTime, Duration, Utc};
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{NewWorkClaim, WorkClaim};
use crate::pool::map_query_err;

const CLAIM_COLS: &str = r#"
    w.id, w.workspace_id, w.kind, w.payload, w.status, w.lease_owner, w.lease_token,
    w.lease_expires_at, w.attempts, w.max_attempts, w.available_at, w.last_error,
    w.created_at, w.updated_at, w.version
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

/// Claim the next ready spawn or signal whose payload is addressed to `host_id`.
///
/// `workspace_id` is ignored: cluster hosts lease jobs in other workspaces.
/// Regular paired hosts must use [`claim_next_spawn_for_host_in_workspace`].
pub async fn claim_next_spawn_for_host(
    pool: &PgPool,
    workspace_id: Uuid,
    host_id: Uuid,
    lease_for: Duration,
) -> DbResult<Option<WorkClaim>> {
    let _ = workspace_id;
    claim_next_spawn_for_host_filtered(pool, host_id, lease_for, None).await
}

/// Home-workspace only. A regular paired box must not take another
/// workspace's `spawn_session` just because the payload names its id.
pub async fn claim_next_spawn_for_host_in_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    host_id: Uuid,
    lease_for: Duration,
) -> DbResult<Option<WorkClaim>> {
    claim_next_spawn_for_host_filtered(pool, host_id, lease_for, Some(workspace_id)).await
}

async fn claim_next_spawn_for_host_filtered(
    pool: &PgPool,
    host_id: Uuid,
    lease_for: Duration,
    workspace_id: Option<Uuid>,
) -> DbResult<Option<WorkClaim>> {
    let lease_token = Uuid::now_v7();
    let lease_expires = Utc::now() + lease_for;
    let host_id_text = host_id.to_string();
    let q = format!(
        r#"
        WITH cte AS (
            SELECT id FROM work_claims
            WHERE status = 'ready'
              AND available_at <= now()
              AND kind IN ('spawn_session', 'signal_session', 'chat_turn')
              AND payload->>'host_id' = $4
              AND ($5::uuid IS NULL OR workspace_id = $5)
            ORDER BY CASE
                       WHEN kind = 'signal_session' THEN 0
                       WHEN kind = 'chat_turn' THEN 1
                       ELSE 2
                     END,
                     available_at, created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE work_claims w
        SET status = 'leased',
            lease_owner = $1,
            lease_token = $2,
            lease_expires_at = $3,
            attempts = attempts + 1,
            version = version + 1,
            updated_at = now()
        FROM cte
        WHERE w.id = cte.id
        RETURNING {CLAIM_COLS}
        "#
    );

    sqlx::query_as::<_, WorkClaim>(&q)
        .bind(host_id)
        .bind(lease_token)
        .bind(lease_expires)
        .bind(&host_id_text)
        .bind(workspace_id)
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

pub async fn get(pool: &PgPool, claim_id: Uuid) -> DbResult<Option<WorkClaim>> {
    let q = format!("SELECT {CLAIM_COLS} FROM work_claims w WHERE w.id = $1");
    sqlx::query_as::<_, WorkClaim>(&q)
        .bind(claim_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn complete(pool: &PgPool, claim_id: Uuid, lease_token: Uuid) -> DbResult<bool> {
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

/// Complete only if this host still holds the lease.
/// `workspace_id` is the **claim's** workspace (cluster hosts complete jobs
/// they leased in other workspaces).
pub async fn complete_owned(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
    host_id: Uuid,
    workspace_id: Uuid,
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
          AND lease_owner = $3
          AND workspace_id = $4
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .bind(host_id)
    .bind(workspace_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// Complete a lease and merge `result` into `payload` (chat_turn stdout).
pub async fn complete_owned_with_result(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
    host_id: Uuid,
    workspace_id: Uuid,
    result: Option<JsonValue>,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET status = 'done',
            payload = CASE
              WHEN $5::jsonb IS NULL THEN payload
              ELSE payload || jsonb_build_object('result', $5::jsonb)
            END,
            lease_owner = NULL,
            lease_token = NULL,
            lease_expires_at = NULL,
            version = version + 1,
            updated_at = now()
        WHERE id = $1
          AND lease_token = $2
          AND status = 'leased'
          AND lease_owner = $3
          AND workspace_id = $4
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .bind(host_id)
    .bind(workspace_id)
    .bind(result)
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

/// Fail only if this host still holds the lease in this workspace.
pub async fn fail_owned(
    pool: &PgPool,
    claim_id: Uuid,
    lease_token: Uuid,
    host_id: Uuid,
    workspace_id: Uuid,
    error: &str,
    retry_after: Option<DateTime<Utc>>,
) -> DbResult<bool> {
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
          AND lease_owner = $5
          AND workspace_id = $6
        "#,
    )
    .bind(claim_id)
    .bind(lease_token)
    .bind(error)
    .bind(retry_after)
    .bind(host_id)
    .bind(workspace_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// A failed spawn must not stay leaseable: the host can come back minutes
/// later and run the old session against a retried task.
pub async fn dead_for_session(pool: &PgPool, session_id: Uuid) -> DbResult<u64> {
    let sid = session_id.to_string();
    let res = sqlx::query(
        r#"
        UPDATE work_claims
        SET status = 'dead',
            last_error = 'session spawn failed',
            lease_owner = NULL,
            lease_token = NULL,
            lease_expires_at = NULL,
            version = version + 1,
            updated_at = now()
        WHERE kind IN ('spawn_session', 'signal_session')
          AND payload->>'session_id' = $1
          AND status IN ('ready', 'leased')
        "#,
    )
    .bind(&sid)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
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
