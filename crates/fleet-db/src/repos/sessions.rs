use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{AuthSession, NewAuthSession};
use crate::pool::map_query_err;

const SESSION_COLS: &str = r#"
    id, user_id, refresh_token_hash, family_id, user_agent,
    created_at, expires_at, absolute_expires_at, revoked_at, last_seen_at
"#;

pub async fn insert(pool: &PgPool, new: &NewAuthSession) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO auth_sessions (
            id, user_id, refresh_token_hash, family_id,
            user_agent, ip, expires_at, absolute_expires_at, last_seen_at
        )
        VALUES ($1, $2, $3, $4, $5, $6::inet, $7, $8, now())
        "#,
    )
    .bind(new.id)
    .bind(new.user_id)
    .bind(&new.refresh_token_hash)
    .bind(new.family_id)
    .bind(&new.user_agent)
    .bind(&new.ip)
    .bind(new.expires_at)
    .bind(new.absolute_expires_at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn insert_tx(tx: &mut Transaction<'_, Postgres>, new: &NewAuthSession) -> DbResult<()> {
    sqlx::query(
        r#"
        INSERT INTO auth_sessions (
            id, user_id, refresh_token_hash, family_id,
            user_agent, ip, expires_at, absolute_expires_at, last_seen_at
        )
        VALUES ($1, $2, $3, $4, $5, $6::inet, $7, $8, now())
        "#,
    )
    .bind(new.id)
    .bind(new.user_id)
    .bind(&new.refresh_token_hash)
    .bind(new.family_id)
    .bind(&new.user_agent)
    .bind(&new.ip)
    .bind(new.expires_at)
    .bind(new.absolute_expires_at)
    .execute(&mut **tx)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn find_by_refresh_hash(
    pool: &PgPool,
    refresh_token_hash: &[u8],
) -> DbResult<Option<AuthSession>> {
    let q = format!("SELECT {SESSION_COLS} FROM auth_sessions WHERE refresh_token_hash = $1");
    sqlx::query_as::<_, AuthSession>(&q)
        .bind(refresh_token_hash)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Single-row revoke by refresh hash (logout). No table-wide locks.
pub async fn revoke_by_refresh_hash(
    pool: &PgPool,
    refresh_token_hash: &[u8],
    at: DateTime<Utc>,
) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE auth_sessions
        SET revoked_at = $1
        WHERE refresh_token_hash = $2
          AND revoked_at IS NULL
        "#,
    )
    .bind(at)
    .bind(refresh_token_hash)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

pub async fn revoke_by_id(pool: &PgPool, id: Uuid, at: DateTime<Utc>) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE auth_sessions
        SET revoked_at = $1
        WHERE id = $2 AND revoked_at IS NULL
        "#,
    )
    .bind(at)
    .bind(id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

/// Revoke every active session in a rotation family (token reuse detection).
pub async fn revoke_family(pool: &PgPool, family_id: Uuid, at: DateTime<Utc>) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE auth_sessions
        SET revoked_at = COALESCE(revoked_at, $1)
        WHERE family_id = $2
          AND revoked_at IS NULL
        "#,
    )
    .bind(at)
    .bind(family_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

/// Optimistic revoke for refresh rotation — must match id + hash + still active.
/// Returns rows affected (0 = concurrent rotation already won).
pub async fn revoke_for_rotation_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    refresh_token_hash: &[u8],
    at: DateTime<Utc>,
) -> DbResult<u64> {
    let res = sqlx::query(
        r#"
        UPDATE auth_sessions
        SET revoked_at = $1
        WHERE id = $2
          AND revoked_at IS NULL
          AND refresh_token_hash = $3
        "#,
    )
    .bind(at)
    .bind(id)
    .bind(refresh_token_hash)
    .execute(&mut **tx)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected())
}

pub async fn is_session_active(pool: &PgPool, session_id: Uuid, user_id: Uuid) -> DbResult<bool> {
    let active = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM auth_sessions
            WHERE id = $1
              AND user_id = $2
              AND revoked_at IS NULL
              AND expires_at > now()
              AND absolute_expires_at > now()
        )
        "#,
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)?;
    Ok(active)
}
