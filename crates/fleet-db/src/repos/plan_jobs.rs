use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::pool::map_query_err;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PlanJob {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub created_by: Option<Uuid>,
    pub goal: String,
    pub context: serde_json::Value,
    pub status: String,
    pub plan_id: Option<Uuid>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewPlanJob {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub created_by: Option<Uuid>,
    pub goal: String,
    pub context: serde_json::Value,
}

pub async fn insert(pool: &PgPool, new: &NewPlanJob) -> DbResult<PlanJob> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        INSERT INTO plan_jobs (id, workspace_id, created_by, goal, context, status)
        VALUES ($1, $2, $3, $4, $5, 'pending')
        RETURNING id, workspace_id, created_by, goal, context, status, plan_id, error,
                  created_at, updated_at
        "#,
    )
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(new.created_by)
    .bind(&new.goal)
    .bind(&new.context)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

pub async fn get(pool: &PgPool, workspace_id: Uuid, id: Uuid) -> DbResult<Option<PlanJob>> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        SELECT id, workspace_id, created_by, goal, context, status, plan_id, error,
               created_at, updated_at
        FROM plan_jobs
        WHERE workspace_id = $1 AND id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn list_for_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    limit: i64,
) -> DbResult<Vec<PlanJob>> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        SELECT id, workspace_id, created_by, goal, context, status, plan_id, error,
               created_at, updated_at
        FROM plan_jobs
        WHERE workspace_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(workspace_id)
    .bind(limit.clamp(1, 100))
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn mark_planning(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> DbResult<Option<PlanJob>> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        UPDATE plan_jobs
        SET status = 'planning', updated_at = now()
        WHERE workspace_id = $1 AND id = $2 AND status = 'pending'
        RETURNING id, workspace_id, created_by, goal, context, status, plan_id, error,
                  created_at, updated_at
        "#,
    )
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn mark_accepted(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    plan_id: Uuid,
) -> DbResult<Option<PlanJob>> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        UPDATE plan_jobs
        SET status = 'accepted', plan_id = $3, error = NULL, updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING id, workspace_id, created_by, goal, context, status, plan_id, error,
                  created_at, updated_at
        "#,
    )
    .bind(workspace_id)
    .bind(id)
    .bind(plan_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn mark_failed(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    error: &str,
) -> DbResult<Option<PlanJob>> {
    sqlx::query_as::<_, PlanJob>(
        r#"
        UPDATE plan_jobs
        SET status = 'failed', error = $3, updated_at = now()
        WHERE workspace_id = $1 AND id = $2
        RETURNING id, workspace_id, created_by, goal, context, status, plan_id, error,
                  created_at, updated_at
        "#,
    )
    .bind(workspace_id)
    .bind(id)
    .bind(error)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}
