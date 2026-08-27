//! Durable goals + policy approvals.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{PolicyApproval, WorkforceGoal};
use crate::pool::map_query_err;

const GOAL_COLS: &str = r#"
    id, workspace_id, title, goal_md, status, run_id, case_id,
    created_by, last_error, created_at, updated_at
"#;

const APPROVAL_COLS: &str = r#"
    id, workspace_id, goal_id, task_id, kind, provider, operation, args,
    summary, status, result, decided_by, decided_at, created_at, updated_at
"#;

#[derive(Debug, Clone)]
pub struct NewGoal {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub goal_md: String,
    pub run_id: Option<Uuid>,
    pub case_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewApproval {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub kind: String,
    pub provider: String,
    pub operation: String,
    pub args: Value,
    pub summary: String,
}

pub async fn insert_goal(pool: &PgPool, spec: &NewGoal) -> DbResult<WorkforceGoal> {
    let q = format!(
        r#"
        INSERT INTO workforce_goals (
            id, workspace_id, title, goal_md, status, run_id, case_id, created_by
        )
        VALUES ($1, $2, $3, $4, 'open', $5, $6, $7)
        RETURNING {GOAL_COLS}
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(spec.id)
        .bind(spec.workspace_id)
        .bind(&spec.title)
        .bind(&spec.goal_md)
        .bind(spec.run_id)
        .bind(spec.case_id)
        .bind(spec.created_by)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get_goal(
    pool: &PgPool,
    workspace_id: Uuid,
    goal_id: Uuid,
) -> DbResult<Option<WorkforceGoal>> {
    let q = format!(
        r#"
        SELECT {GOAL_COLS}
        FROM workforce_goals
        WHERE workspace_id = $1 AND id = $2
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .bind(goal_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_goals(
    pool: &PgPool,
    workspace_id: Uuid,
    status_filter: Option<&str>,
    limit: i64,
) -> DbResult<Vec<WorkforceGoal>> {
    let limit = limit.clamp(1, 200);
    if let Some(st) = status_filter {
        let q = format!(
            r#"
            SELECT {GOAL_COLS}
            FROM workforce_goals
            WHERE workspace_id = $1 AND status = $2
            ORDER BY updated_at DESC
            LIMIT $3
            "#
        );
        sqlx::query_as::<_, WorkforceGoal>(&q)
            .bind(workspace_id)
            .bind(st)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_query_err)
    } else {
        let q = format!(
            r#"
            SELECT {GOAL_COLS}
            FROM workforce_goals
            WHERE workspace_id = $1
            ORDER BY updated_at DESC
            LIMIT $2
            "#
        );
        sqlx::query_as::<_, WorkforceGoal>(&q)
            .bind(workspace_id)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_query_err)
    }
}

pub async fn list_open_goals(pool: &PgPool, workspace_id: Uuid) -> DbResult<Vec<WorkforceGoal>> {
    let q = format!(
        r#"
        SELECT {GOAL_COLS}
        FROM workforce_goals
        WHERE workspace_id = $1
          AND status IN ('open', 'acting', 'waiting_approval')
        ORDER BY updated_at DESC
        LIMIT 50
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn attach_run(
    pool: &PgPool,
    workspace_id: Uuid,
    goal_id: Uuid,
    run_id: Uuid,
) -> DbResult<Option<WorkforceGoal>> {
    let q = format!(
        r#"
        UPDATE workforce_goals
        SET run_id = $3,
            status = CASE
                WHEN status IN ('completed', 'failed', 'cancelled') THEN status
                ELSE 'acting'
            END,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING {GOAL_COLS}
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .bind(goal_id)
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn attach_case(
    pool: &PgPool,
    workspace_id: Uuid,
    goal_id: Uuid,
    case_id: Uuid,
) -> DbResult<Option<WorkforceGoal>> {
    let q = format!(
        r#"
        UPDATE workforce_goals
        SET case_id = $3,
            status = CASE
                WHEN status IN ('completed', 'failed', 'cancelled') THEN status
                ELSE 'acting'
            END,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING {GOAL_COLS}
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .bind(goal_id)
        .bind(case_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn set_goal_status(
    pool: &PgPool,
    workspace_id: Uuid,
    goal_id: Uuid,
    status: &str,
    last_error: Option<&str>,
) -> DbResult<Option<WorkforceGoal>> {
    let q = format!(
        r#"
        UPDATE workforce_goals
        SET status = $3,
            last_error = $4,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING {GOAL_COLS}
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .bind(goal_id)
        .bind(status)
        .bind(last_error)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Latest open goal in the workspace (for attaching a new run/case).
pub async fn latest_open_goal(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Option<WorkforceGoal>> {
    let q = format!(
        r#"
        SELECT {GOAL_COLS}
        FROM workforce_goals
        WHERE workspace_id = $1
          AND status IN ('open', 'acting', 'waiting_approval')
        ORDER BY updated_at DESC
        LIMIT 1
        "#
    );
    sqlx::query_as::<_, WorkforceGoal>(&q)
        .bind(workspace_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn insert_approval(pool: &PgPool, spec: &NewApproval) -> DbResult<PolicyApproval> {
    let q = format!(
        r#"
        INSERT INTO policy_approvals (
            id, workspace_id, goal_id, task_id, kind, provider, operation,
            args, summary, status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending')
        RETURNING {APPROVAL_COLS}
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(spec.id)
        .bind(spec.workspace_id)
        .bind(spec.goal_id)
        .bind(spec.task_id)
        .bind(&spec.kind)
        .bind(&spec.provider)
        .bind(&spec.operation)
        .bind(&spec.args)
        .bind(&spec.summary)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get_approval(
    pool: &PgPool,
    workspace_id: Uuid,
    approval_id: Uuid,
) -> DbResult<Option<PolicyApproval>> {
    let q = format!(
        r#"
        SELECT {APPROVAL_COLS}
        FROM policy_approvals
        WHERE workspace_id = $1 AND id = $2
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(workspace_id)
        .bind(approval_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_pending_approvals(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Vec<PolicyApproval>> {
    let q = format!(
        r#"
        SELECT {APPROVAL_COLS}
        FROM policy_approvals
        WHERE workspace_id = $1 AND status = 'pending'
        ORDER BY created_at ASC
        LIMIT 100
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_approvals_for_goal(
    pool: &PgPool,
    workspace_id: Uuid,
    goal_id: Uuid,
) -> DbResult<Vec<PolicyApproval>> {
    let q = format!(
        r#"
        SELECT {APPROVAL_COLS}
        FROM policy_approvals
        WHERE workspace_id = $1 AND goal_id = $2
        ORDER BY created_at DESC
        LIMIT 50
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(workspace_id)
        .bind(goal_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn decide_approval(
    pool: &PgPool,
    workspace_id: Uuid,
    approval_id: Uuid,
    status: &str,
    decided_by: Uuid,
) -> DbResult<Option<PolicyApproval>> {
    let q = format!(
        r#"
        UPDATE policy_approvals
        SET status = $3,
            decided_by = $4,
            decided_at = now(),
            updated_at = now()
        WHERE workspace_id = $1
          AND id = $2
          AND status = 'pending'
          AND $3 IN ('approved', 'denied')
        RETURNING {APPROVAL_COLS}
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(workspace_id)
        .bind(approval_id)
        .bind(status)
        .bind(decided_by)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn finish_approval(
    pool: &PgPool,
    workspace_id: Uuid,
    approval_id: Uuid,
    status: &str,
    result: &Value,
) -> DbResult<Option<PolicyApproval>> {
    let q = format!(
        r#"
        UPDATE policy_approvals
        SET status = $3,
            result = $4,
            updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING {APPROVAL_COLS}
        "#
    );
    sqlx::query_as::<_, PolicyApproval>(&q)
        .bind(workspace_id)
        .bind(approval_id)
        .bind(status)
        .bind(result)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn pending_count(pool: &PgPool, workspace_id: Uuid) -> DbResult<i64> {
    let n: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)::bigint
        FROM policy_approvals
        WHERE workspace_id = $1 AND status = 'pending'
        "#,
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)?;
    Ok(n.0)
}

/// Block the newest live board task so the human sees it on the kanban.
pub async fn block_latest_live_task(
    pool: &PgPool,
    workspace_id: Uuid,
    reason: &str,
    notes: &str,
) -> DbResult<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        UPDATE board_tasks
        SET status = 'blocked',
            block_reason = $2,
            hitl_notes = CASE
                WHEN hitl_notes IS NULL OR hitl_notes = '' THEN $3
                ELSE hitl_notes || E'\n\n' || $3
            END,
            updated_at = now()
        WHERE id = (
            SELECT id FROM board_tasks
            WHERE project_id = $1
              AND status IN ('assigned', 'in_progress', 'ready')
            ORDER BY updated_at DESC
            LIMIT 1
        )
        RETURNING id
        "#,
    )
    .bind(workspace_id)
    .bind(reason)
    .bind(notes)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    Ok(row.map(|r| r.0))
}

/// After a policy deny/approve, reopen the blocked task if it was waiting on this gate.
pub async fn resume_blocked_task(
    pool: &PgPool,
    workspace_id: Uuid,
    task_id: Uuid,
    note: &str,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE board_tasks
        SET status = 'ready',
            hitl_resolved_at = now(),
            hitl_resume_requested_at = NULL,
            claimed_session_id = NULL,
            block_reason = NULL,
            hitl_notes = CASE
                WHEN hitl_notes IS NULL OR hitl_notes = '' THEN $3
                ELSE hitl_notes || E'\n\n' || $3
            END,
            updated_at = now()
        WHERE project_id = $1 AND id = $2 AND status = 'blocked'
        "#,
    )
    .bind(workspace_id)
    .bind(task_id)
    .bind(note)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

pub async fn reopen_task_ready(
    pool: &PgPool,
    workspace_id: Uuid,
    task_id: Uuid,
    reason: &str,
    notes: &str,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE board_tasks
        SET status = 'ready',
            claimed_session_id = NULL,
            terminal_at = NULL,
            failure_reason = $3,
            hitl_notes = CASE
                WHEN hitl_notes IS NULL OR hitl_notes = '' THEN $4
                ELSE hitl_notes || E'\n\n' || $4
            END,
            updated_at = now()
        WHERE project_id = $1
          AND id = $2
          AND status NOT IN ('cancelled', 'done_failed')
        "#,
    )
    .bind(workspace_id)
    .bind(task_id)
    .bind(reason)
    .bind(notes)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}

/// Keep a goal alive when its run dies; complete only when work + approvals are done.
pub async fn reconcile_goal(
    pool: &PgPool,
    workspace_id: Uuid,
    goal: &WorkforceGoal,
) -> DbResult<WorkforceGoal> {
    if matches!(
        goal.status.as_str(),
        "completed" | "failed" | "cancelled"
    ) {
        return Ok(goal.clone());
    }

    if let Some(case_id) = goal.case_id {
        let st: Option<(String,)> =
            sqlx::query_as("SELECT status FROM cases WHERE id = $1 AND workspace_id = $2")
                .bind(case_id)
                .bind(workspace_id)
                .fetch_optional(pool)
                .await
                .map_err(map_query_err)?;
        if let Some((status,)) = st {
            let mapped = match status.as_str() {
                "completed" => Some("completed"),
                "failed" => Some("failed"),
                "cancelled" => Some("cancelled"),
                "acting" | "deciding" | "waiting" | "open" => Some("acting"),
                _ => None,
            };
            if let Some(next) = mapped {
                return set_goal_status(pool, workspace_id, goal.id, next, None)
                    .await?
                    .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()));
            }
        }
    }

    if let Some(run_id) = goal.run_id {
        let run: Option<(String,)> =
            sqlx::query_as("SELECT status FROM work_runs WHERE id = $1 AND project_id = $2")
                .bind(run_id)
                .bind(workspace_id)
                .fetch_optional(pool)
                .await
                .map_err(map_query_err)?;
        let tasks: Vec<(String,)> = sqlx::query_as(
            r#"
            SELECT status FROM board_tasks
            WHERE project_id = $1 AND run_id = $2
            "#,
        )
        .bind(workspace_id)
        .bind(run_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)?;

        let all_success = !tasks.is_empty()
            && tasks.iter().all(|(s,)| s == "done_success");
        let any_open = tasks.iter().any(|(s,)| {
            !matches!(
                s.as_str(),
                "done_success" | "done_failed" | "cancelled"
            )
        });
        let run_st = run.as_ref().map(|(s,)| s.as_str()).unwrap_or("");

        if all_success {
            return set_goal_status(pool, workspace_id, goal.id, "completed", None)
                .await?
                .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()));
        }
        if any_open {
            // Run may be failed — keep the goal. Agents are disposable.
            return set_goal_status(pool, workspace_id, goal.id, "acting", None)
                .await?
                .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()));
        }
        if matches!(run_st, "failed" | "cancelled") && !any_open {
            let next = if run_st == "cancelled" {
                "cancelled"
            } else {
                "failed"
            };
            return set_goal_status(pool, workspace_id, goal.id, next, None)
                .await?
                .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()));
        }
        return set_goal_status(pool, workspace_id, goal.id, "acting", None)
            .await?
            .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()));
    }

    set_goal_status(pool, workspace_id, goal.id, "open", None)
        .await?
        .ok_or_else(|| DbError::NotFound("goal vanished during reconcile".into()))
}

pub async fn reconcile_workspace(pool: &PgPool, workspace_id: Uuid) -> DbResult<Vec<WorkforceGoal>> {
    let open = list_open_goals(pool, workspace_id).await?;
    let mut out = Vec::with_capacity(open.len());
    for g in open {
        out.push(reconcile_goal(pool, workspace_id, &g).await?);
    }
    Ok(out)
}
