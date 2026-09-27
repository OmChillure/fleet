//! Singleton instance settings (id = 1).

use sqlx::PgPool;

use crate::{DbError, DbResult};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InstanceSettings {
    pub max_agents_cap: i32,
}

pub async fn get(pool: &PgPool) -> DbResult<InstanceSettings> {
    sqlx::query_as::<_, InstanceSettings>(
        r#"
        SELECT max_agents_cap
        FROM instance_settings
        WHERE id = 1
        "#,
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| DbError::NotFound("instance_settings".into()))
}

pub async fn set_max_agents_cap(pool: &PgPool, cap: i32) -> DbResult<InstanceSettings> {
    let cap = cap.clamp(1, 1024);
    sqlx::query_as::<_, InstanceSettings>(
        r#"
        UPDATE instance_settings
        SET max_agents_cap = $1, updated_at = now()
        WHERE id = 1
        RETURNING max_agents_cap
        "#,
    )
    .bind(cap)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| DbError::NotFound("instance_settings".into()))
}
