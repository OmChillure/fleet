//! Durable general-purpose cases (state + wake schedule).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{Case, CaseEvent};
use crate::pool::map_query_err;

const CASE_COLS: &str = r#"
    id, workspace_id, kind, title, policy_md, status, stage, context,
    next_wake_at, last_error, run_id, decision_count, created_by, version,
    created_at, updated_at
"#;

const CASE_COLS_C: &str = r#"
    c.id, c.workspace_id, c.kind, c.title, c.policy_md, c.status, c.stage, c.context,
    c.next_wake_at, c.last_error, c.run_id, c.decision_count, c.created_by, c.version,
    c.created_at, c.updated_at
"#;

const EVENT_COLS: &str = r#"
    id, case_id, workspace_id, kind, payload, created_at
"#;

#[derive(Debug, Clone)]
pub struct NewCase {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub title: String,
    pub policy_md: String,
    pub stage: String,
    pub context: Value,
    pub next_wake_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
}

pub async fn insert(pool: &PgPool, spec: &NewCase) -> DbResult<Case> {
    let q = format!(
        r#"
        INSERT INTO cases (
            id, workspace_id, kind, title, policy_md, status, stage, context,
            next_wake_at, created_by
        )
        VALUES ($1, $2, $3, $4, $5, 'open', $6, $7, $8, $9)
        RETURNING {CASE_COLS}
        "#
    );
    sqlx::query_as::<_, Case>(&q)
        .bind(spec.id)
        .bind(spec.workspace_id)
        .bind(&spec.kind)
        .bind(&spec.title)
        .bind(&spec.policy_md)
        .bind(&spec.stage)
        .bind(&spec.context)
        .bind(spec.next_wake_at)
        .bind(spec.created_by)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get(pool: &PgPool, workspace_id: Uuid, case_id: Uuid) -> DbResult<Option<Case>> {
    let q = format!(
        r#"
        SELECT {CASE_COLS}
        FROM cases
        WHERE workspace_id = $1 AND id = $2
        "#
    );
    sqlx::query_as::<_, Case>(&q)
        .bind(workspace_id)
        .bind(case_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_for_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    status_filter: Option<&str>,
    limit: i64,
) -> DbResult<Vec<Case>> {
    let limit = limit.clamp(1, 200);
    if let Some(st) = status_filter {
        let q = format!(
            r#"
            SELECT {CASE_COLS}
            FROM cases
            WHERE workspace_id = $1 AND status = $2
            ORDER BY updated_at DESC
            LIMIT $3
            "#
        );
        sqlx::query_as::<_, Case>(&q)
            .bind(workspace_id)
            .bind(st)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_query_err)
    } else {
        let q = format!(
            r#"
            SELECT {CASE_COLS}
            FROM cases
            WHERE workspace_id = $1
            ORDER BY updated_at DESC
            LIMIT $2
            "#
        );
        sqlx::query_as::<_, Case>(&q)
            .bind(workspace_id)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_query_err)
    }
}

/// Claim due cases for decision (status open/waiting, next_wake_at <= now).
pub async fn claim_due(pool: &PgPool, limit: i64) -> DbResult<Vec<Case>> {
    let limit = limit.clamp(1, 50);
    let q = format!(
        r#"
        WITH due AS (
            SELECT id
            FROM cases
            WHERE status IN ('open', 'waiting')
              AND next_wake_at IS NOT NULL
              AND next_wake_at <= now()
            ORDER BY next_wake_at ASC
            LIMIT $1
            FOR UPDATE SKIP LOCKED
        ),
        upd AS (
            UPDATE cases AS c
            SET status = 'deciding',
                version = c.version + 1,
                updated_at = now(),
                last_error = NULL
            FROM due
            WHERE c.id = due.id
            RETURNING {CASE_COLS_C}
        )
        SELECT {CASE_COLS}
        FROM upd
        ORDER BY next_wake_at ASC NULLS LAST
        "#
    );
    sqlx::query_as::<_, Case>(&q)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_acting_with_run(pool: &PgPool, limit: i64) -> DbResult<Vec<Case>> {
    let limit = limit.clamp(1, 100);
    let q = format!(
        r#"
        SELECT {CASE_COLS}
        FROM cases
        WHERE status = 'acting' AND run_id IS NOT NULL
        ORDER BY updated_at ASC
        LIMIT $1
        "#
    );
    sqlx::query_as::<_, Case>(&q)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

#[derive(Debug, Clone)]
pub struct CasePatch {
    pub status: String,
    pub stage: String,
    pub context: Value,
    pub next_wake_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub run_id: Option<Uuid>,
    pub clear_run_id: bool,
    pub bump_decision: bool,
    pub expected_version: i64,
}

pub async fn apply_patch(
    pool: &PgPool,
    workspace_id: Uuid,
    case_id: Uuid,
    patch: &CasePatch,
) -> DbResult<Case> {
    let q = format!(
        r#"
        UPDATE cases
        SET status = $3,
            stage = $4,
            context = $5,
            next_wake_at = $6,
            last_error = $7,
            run_id = CASE WHEN $8 THEN NULL ELSE COALESCE($9, run_id) END,
            decision_count = decision_count + CASE WHEN $10 THEN 1 ELSE 0 END,
            version = version + 1,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2 AND version = $11
        RETURNING {CASE_COLS}
        "#
    );
    let row = sqlx::query_as::<_, Case>(&q)
        .bind(workspace_id)
        .bind(case_id)
        .bind(&patch.status)
        .bind(&patch.stage)
        .bind(&patch.context)
        .bind(patch.next_wake_at)
        .bind(&patch.last_error)
        .bind(patch.clear_run_id)
        .bind(patch.run_id)
        .bind(patch.bump_decision)
        .bind(patch.expected_version)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?;
    row.ok_or_else(|| DbError::Conflict("case version conflict or not found".into()))
}

pub async fn append_event(
    pool: &PgPool,
    case_id: Uuid,
    workspace_id: Uuid,
    kind: &str,
    payload: Value,
) -> DbResult<CaseEvent> {
    let q = format!(
        r#"
        INSERT INTO case_events (case_id, workspace_id, kind, payload)
        VALUES ($1, $2, $3, $4)
        RETURNING {EVENT_COLS}
        "#
    );
    sqlx::query_as::<_, CaseEvent>(&q)
        .bind(case_id)
        .bind(workspace_id)
        .bind(kind)
        .bind(payload)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_events(
    pool: &PgPool,
    case_id: Uuid,
    limit: i64,
) -> DbResult<Vec<CaseEvent>> {
    let limit = limit.clamp(1, 200);
    let q = format!(
        r#"
        SELECT {EVENT_COLS}
        FROM case_events
        WHERE case_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#
    );
    sqlx::query_as::<_, CaseEvent>(&q)
        .bind(case_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

/// Force wake: set status open and next_wake_at = now (non-terminal only).
pub async fn schedule_wake_now(
    pool: &PgPool,
    workspace_id: Uuid,
    case_id: Uuid,
) -> DbResult<Option<Case>> {
    let q = format!(
        r#"
        UPDATE cases
        SET status = CASE
                WHEN status IN ('completed', 'failed', 'cancelled') THEN status
                WHEN status = 'acting' THEN status
                ELSE 'open'
            END,
            next_wake_at = CASE
                WHEN status IN ('completed', 'failed', 'cancelled', 'acting') THEN next_wake_at
                ELSE now()
            END,
            version = version + 1,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING {CASE_COLS}
        "#
    );
    sqlx::query_as::<_, Case>(&q)
        .bind(workspace_id)
        .bind(case_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}
