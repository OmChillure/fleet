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

/// Wire statuses `claim_due` (and therefore `process_due`) may pick.
///
/// A leftover due `next_wake_at` on completed/cancelled/failed/acting/deciding
/// must not flip the row to `deciding`. Keep the SQL `status IN (...)` in
/// `claim_due` identical to this list.
pub const CLAIM_DUE_STATUSES: &[&str] = &["open", "waiting"];

/// `true` only for statuses `claim_due` / `process_due` may select.
pub fn is_claim_due_status(status: &str) -> bool {
    CLAIM_DUE_STATUSES.iter().copied().any(|s| s == status)
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

/// Release cases left in `deciding` after a crash or a hung LLM call.
///
/// `updated_at` is bumped on claim, so in-flight decides younger than
/// `stale_after_secs` are left alone.
pub async fn reclaim_stale_deciding(pool: &PgPool, stale_after_secs: i64) -> DbResult<u64> {
    let stale_after_secs = stale_after_secs.clamp(60, 3600);
    let res = sqlx::query(
        r#"
        UPDATE cases
        SET status = 'waiting',
            next_wake_at = now(),
            last_error = COALESCE(last_error, 'stale deciding reclaim'),
            version = version + 1,
            updated_at = now()
        WHERE status = 'deciding'
          AND updated_at < now() - make_interval(secs => $1)
        "#,
    )
    .bind(stale_after_secs)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
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
          AND status NOT IN ('completed', 'cancelled', 'failed')
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

pub async fn list_events(pool: &PgPool, case_id: Uuid, limit: i64) -> DbResult<Vec<CaseEvent>> {
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

#[cfg(test)]
mod process_due_claim_due_tests {
    use super::*;

    fn production_src() -> &'static str {
        let src = include_str!("cases.rs");
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

    /// A due `next_wake_at` on a terminal row must not be a claim source.
    #[test]
    fn process_due_claim_due_statuses_exclude_completed_cancelled_failed() {
        for terminal in ["completed", "cancelled", "failed"] {
            assert!(
                !is_claim_due_status(terminal),
                "process_due/claim_due must not pick {terminal} even when next_wake_at is due"
            );
            assert!(!CLAIM_DUE_STATUSES.contains(&terminal));
        }
        assert!(is_claim_due_status("open"));
        assert!(is_claim_due_status("waiting"));
        assert_eq!(CLAIM_DUE_STATUSES, &["open", "waiting"]);
        for inflight in ["acting", "deciding"] {
            assert!(
                !is_claim_due_status(inflight),
                "{inflight} is not a process_due claim source"
            );
        }
    }

    /// SQL `WHERE` must AND the open/waiting filter with due-wake.
    /// `status IN ('open', 'waiting') OR next_wake_at <= now()` would
    /// revive a due completed row. Not the product lock — see cases_repos.
    #[test]
    fn process_due_claim_due_sql_skips_terminal_statuses() {
        let claim = fn_src(production_src(), "pub async fn claim_due");
        assert!(
            claim.contains("WHERE status IN ('open', 'waiting')"),
            "claim_due must only SELECT open/waiting; got:\n{claim}"
        );
        assert!(
            claim.contains("AND next_wake_at IS NOT NULL"),
            "due-wake must be AND-ed with the status filter:\n{claim}"
        );
        assert!(
            claim.contains("AND next_wake_at <= now()"),
            "due-wake must be AND-ed with the status filter:\n{claim}"
        );
        assert!(
            !claim.contains("OR next_wake_at"),
            "OR-ing next_wake_at with the status IN-list would revive a due terminal:\n{claim}"
        );
        assert!(
            claim.contains("SET status = 'deciding'"),
            "claim_due must move claimed rows to deciding"
        );

        let reclaim = fn_src(production_src(), "pub async fn reclaim_stale_deciding");
        assert!(
            reclaim.contains("WHERE status = 'deciding'"),
            "reclaim (process_due's first step) must only touch deciding"
        );
    }

    /// A decide patch (status change + optional `bump_decision`) on
    /// completed/cancelled/failed must not UPDATE the row. Version-only
    /// WHERE would reopen the case. Live lock is in cases_repos.
    #[test]
    fn decide_on_terminal_apply_patch_sql_refuses_reopen() {
        let apply = fn_src(production_src(), "pub async fn apply_patch");
        let where_at = apply
            .find("WHERE ")
            .unwrap_or_else(|| panic!("apply_patch UPDATE needs WHERE:\n{apply}"));
        let where_sql = &apply[where_at..];
        assert!(
            where_sql.contains("version"),
            "apply_patch must stay version-locked:\n{where_sql}"
        );
        for terminal in ["completed", "cancelled", "failed"] {
            assert!(
                where_sql.contains(terminal),
                "apply_patch WHERE must refuse leaving {terminal} so a decide cannot reopen:\n{where_sql}"
            );
        }
        assert!(
            where_sql.contains("status"),
            "version-only WHERE would apply a decide patch to a terminal case:\n{where_sql}"
        );
        assert!(
            where_sql.contains("NOT IN")
                || where_sql.contains("status IN ('completed'")
                || where_sql.contains("status IN ('cancelled'")
                || where_sql.contains("status IN ('failed'"),
            "apply_patch WHERE must predicate on terminal statuses (NOT IN / keep-same):\n{where_sql}"
        );
    }
}
