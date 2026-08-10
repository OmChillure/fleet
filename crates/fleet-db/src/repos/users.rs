use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::User;
use crate::pool::map_query_err;

const USER_COLS: &str = r#"
    id, email, email_normalized, display_name, password_hash,
    status, created_at, updated_at, version
"#;

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> DbResult<Option<User>> {
    let q = format!("SELECT {USER_COLS} FROM users WHERE id = $1");
    sqlx::query_as::<_, User>(&q)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_active_by_id(pool: &PgPool, id: Uuid) -> DbResult<Option<User>> {
    let q = format!("SELECT {USER_COLS} FROM users WHERE id = $1 AND status = 'active'");
    sqlx::query_as::<_, User>(&q)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn find_by_email_normalized(
    pool: &PgPool,
    email_normalized: &str,
) -> DbResult<Option<User>> {
    let q = format!("SELECT {USER_COLS} FROM users WHERE email_normalized = $1");
    sqlx::query_as::<_, User>(&q)
        .bind(email_normalized)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub id: Uuid,
    pub email: String,
    pub email_normalized: String,
    pub display_name: Option<String>,
    pub password_hash: String,
    pub created_at: DateTime<Utc>,
}

/// Insert user inside an open transaction (signup).
pub async fn insert_tx(tx: &mut Transaction<'_, Postgres>, new: &NewUser) -> DbResult<User> {
    let q = format!(
        r#"
        INSERT INTO users (
            id, email, email_normalized, display_name, password_hash,
            status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, 'active', $6, $6)
        RETURNING {USER_COLS}
        "#
    );
    sqlx::query_as::<_, User>(&q)
        .bind(new.id)
        .bind(&new.email)
        .bind(&new.email_normalized)
        .bind(&new.display_name)
        .bind(&new.password_hash)
        .bind(new.created_at)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| {
            // Prefer a domain message over from_sqlx's generic
            // Conflict("unique constraint violated"). Check 23505 before
            // map_query_err; is_unique_violation() is still true for either
            // Conflict form (see DbError::is_unique_violation).
            if let sqlx::Error::Database(ref db) = e
                && db.code().as_deref() == Some("23505")
            {
                return DbError::Conflict("an account with this email already exists".into());
            }
            map_query_err(e)
        })
}
