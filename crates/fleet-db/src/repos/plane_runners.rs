//! Per-workspace control-plane runner settings.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::PlaneRunner;
use crate::pool::map_query_err;

const COLS: &str = "workspace_id, name, allow_host, allow_container, max_concurrent, updated_at";

#[derive(Debug, Clone, Default)]
pub struct PlaneRunnerPatch {
    pub name: Option<String>,
    pub allow_host: Option<bool>,
    pub allow_container: Option<bool>,
    pub max_concurrent: Option<i32>,
}

pub async fn get(pool: &PgPool, workspace_id: Uuid) -> DbResult<Option<PlaneRunner>> {
    sqlx::query_as::<_, PlaneRunner>(&format!(
        "SELECT {COLS} FROM workspace_plane_runners WHERE workspace_id = $1"
    ))
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn get_or_default(pool: &PgPool, workspace_id: Uuid) -> DbResult<PlaneRunner> {
    Ok(get(pool, workspace_id)
        .await?
        .unwrap_or_else(|| PlaneRunner::default_for(workspace_id)))
}

/// Native jobs on this plane. Missing row → today's default (on).
pub async fn taking_native_jobs(pool: &PgPool, workspace_id: Uuid) -> DbResult<bool> {
    Ok(get_or_default(pool, workspace_id)
        .await?
        .taking_native_jobs())
}

pub async fn upsert(
    pool: &PgPool,
    workspace_id: Uuid,
    patch: &PlaneRunnerPatch,
) -> DbResult<PlaneRunner> {
    let current = get_or_default(pool, workspace_id).await?;
    let name = patch
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(current.name.as_str())
        .to_string();
    let allow_host = patch.allow_host.unwrap_or(current.allow_host);
    let allow_container = patch.allow_container.unwrap_or(current.allow_container);
    let max_concurrent = patch
        .max_concurrent
        .unwrap_or(current.max_concurrent)
        .clamp(1, 256);

    sqlx::query_as::<_, PlaneRunner>(&format!(
        r#"
        INSERT INTO workspace_plane_runners
            (workspace_id, name, allow_host, allow_container, max_concurrent, updated_at)
        VALUES ($1, $2, $3, $4, $5, now())
        ON CONFLICT (workspace_id) DO UPDATE SET
            name = EXCLUDED.name,
            allow_host = EXCLUDED.allow_host,
            allow_container = EXCLUDED.allow_container,
            max_concurrent = EXCLUDED.max_concurrent,
            updated_at = now()
        RETURNING {COLS}
        "#
    ))
    .bind(workspace_id)
    .bind(&name)
    .bind(allow_host)
    .bind(allow_container)
    .bind(max_concurrent)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}
