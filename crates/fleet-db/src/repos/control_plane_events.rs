//! Control-plane event outbox (`control_plane_events`).

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::ControlPlaneEvent;
use crate::pool::map_query_err;

const EVENT_COLS: &str = r#"
    id, message_id, "type" AS event_type, body, created_at, published_at
"#;

/// Inbound sample: persist as already published (dedupe on `message_id`).
pub async fn insert_inbound_published(
    pool: &PgPool,
    message_id: Uuid,
    event_type: &str,
    body: serde_json::Value,
) -> DbResult<Option<ControlPlaneEvent>> {
    let id = Uuid::now_v7();
    let q = format!(
        r#"
        INSERT INTO control_plane_events (id, message_id, "type", body, published_at)
        VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (message_id) DO NOTHING
        RETURNING {EVENT_COLS}
        "#
    );
    sqlx::query_as::<_, ControlPlaneEvent>(&q)
        .bind(id)
        .bind(message_id)
        .bind(event_type)
        .bind(body)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Persist a full `BusMessage` for later publish. `message_id` is unique.
pub async fn insert_event(
    pool: &PgPool,
    message_id: Uuid,
    event_type: &str,
    body: serde_json::Value,
) -> DbResult<ControlPlaneEvent> {
    let id = Uuid::now_v7();
    let q = format!(
        r#"
        INSERT INTO control_plane_events (id, message_id, "type", body)
        VALUES ($1, $2, $3, $4)
        RETURNING {EVENT_COLS}
        "#
    );
    sqlx::query_as::<_, ControlPlaneEvent>(&q)
        .bind(id)
        .bind(message_id)
        .bind(event_type)
        .bind(body)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

/// Oldest unpublished rows, up to `limit` (clamped 1..=1000).
pub async fn list_unpublished(pool: &PgPool, limit: i64) -> DbResult<Vec<ControlPlaneEvent>> {
    let limit = limit.clamp(1, 1000);
    let q = format!(
        r#"
        SELECT {EVENT_COLS}
        FROM control_plane_events
        WHERE published_at IS NULL
        ORDER BY created_at ASC, id ASC
        LIMIT $1
        "#
    );
    sqlx::query_as::<_, ControlPlaneEvent>(&q)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

/// Stamp `published_at` when still unpublished. `id` is the outbox row PK.
pub async fn mark_published(pool: &PgPool, id: Uuid) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        UPDATE control_plane_events
        SET published_at = now()
        WHERE id = $1 AND published_at IS NULL
        "#,
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() == 1)
}
