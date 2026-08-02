use super::StoreError;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

pub async fn connect(database_url: &str) -> Result<PgPool, StoreError> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .map_err(StoreError::Sqlx)
}

pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    // ignore_missing: local DBs that applied the old `001_init` version after renumbering.
    sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(pool)
        .await
        .map_err(|e| StoreError::Validation(format!("migrate: {e}")))?;
    Ok(())
}
