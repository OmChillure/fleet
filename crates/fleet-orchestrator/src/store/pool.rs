use super::StoreError;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use std::time::Duration;

/// Same hosted-URL rules as `fleet_db::pool::normalize_database_url`.
fn normalize_database_url(database_url: &str) -> String {
    let mut url = database_url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if lower.contains("pooler.supabase") && lower.contains(":6543") {
        url = url.replace(":6543", ":5432");
    }
    let lower = url.to_ascii_lowercase();
    let hosted = lower.contains("supabase.co")
        || lower.contains("supabase.com")
        || lower.contains("neon.tech")
        || lower.contains("pooler.supabase");
    if hosted && !lower.contains("sslmode=") {
        url = if url.contains('?') {
            format!("{url}&sslmode=require")
        } else {
            format!("{url}?sslmode=require")
        };
    }
    url
}

pub async fn connect(database_url: &str) -> Result<PgPool, StoreError> {
    let url = normalize_database_url(database_url);
    let acquire_secs = std::env::var("DATABASE_ACQUIRE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(5, 300);

    let mut opts = PgConnectOptions::from_str(&url).map_err(StoreError::Sqlx)?;
    let lower = url.to_ascii_lowercase();
    if lower.contains("pooler") || lower.contains("supabase") || lower.contains("pgbouncer=true") {
        opts = opts.statement_cache_capacity(0);
    }

    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(acquire_secs))
        .after_release(|conn, _meta| {
            Box::pin(async move {
                let _ = sqlx::query("SELECT pg_advisory_unlock_all()")
                    .execute(&mut *conn)
                    .await;
                Ok(true)
            })
        })
        .connect_with(opts)
        .await
        .map_err(StoreError::Sqlx)
}

pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Sqlx)?;
    let _ = sqlx::query("DEALLOCATE ALL").execute(&mut *conn).await;
    sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(&mut *conn)
        .await
        .map_err(|e| StoreError::Validation(format!("migrate: {e}")))?;
    Ok(())
}
