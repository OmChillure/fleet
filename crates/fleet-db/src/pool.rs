use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::{DbError, DbResult};

/// Tunables for the shared Postgres pool.
///
/// Sized for many concurrent agents behind Neon/Supabase poolers:
/// keep app-side `max_connections` modest and let the external pooler fan out.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Option<Duration>,
    pub max_lifetime: Option<Duration>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 20,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(600)),
            max_lifetime: Some(Duration::from_secs(1800)),
        }
    }
}

/// Connect without running migrations.
pub async fn connect(database_url: &str) -> DbResult<PgPool> {
    connect_with(database_url, &PoolConfig::default()).await
}

pub async fn connect_with(database_url: &str, cfg: &PoolConfig) -> DbResult<PgPool> {
    let mut opts = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .acquire_timeout(cfg.acquire_timeout);

    if let Some(idle) = cfg.idle_timeout {
        opts = opts.idle_timeout(idle);
    }
    if let Some(life) = cfg.max_lifetime {
        opts = opts.max_lifetime(life);
    }

    match opts.connect(database_url).await {
        Ok(pool) => Ok(pool),
        Err(err) => {
            let target = redact_db_target(database_url);
            tracing::error!(
                target = %target,
                error = %err,
                "failed to connect to Postgres"
            );
            Err(DbError::Connect {
                target,
                source: err,
            })
        }
    }
}

/// Host/db for logs — never includes password.
fn redact_db_target(database_url: &str) -> String {
    // postgres://user:pass@host:port/db?… → host:port/db
    let without_scheme = database_url
        .split("://")
        .nth(1)
        .unwrap_or(database_url);
    let after_at = without_scheme.split('@').nth(1).unwrap_or(without_scheme);
    let host_and_path = after_at.split('?').next().unwrap_or(after_at);
    host_and_path.to_string()
}

/// Apply embedded migrations from this crate (`migrations/`).
///
/// `ignore_missing`: other crates (e.g. fleet-orchestrator) may share the same
/// `_sqlx_migrations` table with additional version rows.
pub async fn migrate(pool: &PgPool) -> DbResult<()> {
    match sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(pool)
        .await
    {
        Ok(()) => {
            tracing::info!("fleet-db migrations applied successfully");
            Ok(())
        }
        Err(err) => {
            tracing::error!(error = %err, "fleet-db migration failed");
            Err(err.into())
        }
    }
}

/// Connect + migrate in one step (typical service startup).
pub async fn connect_and_migrate(database_url: &str) -> DbResult<PgPool> {
    connect_and_migrate_with(database_url, &PoolConfig::default()).await
}

pub async fn connect_and_migrate_with(
    database_url: &str,
    cfg: &PoolConfig,
) -> DbResult<PgPool> {
    let target = redact_db_target(database_url);
    tracing::info!(db_target = %target, "connecting to database…");
    let pool = connect_with(database_url, cfg).await?;
    tracing::info!(db_target = %target, "database connection established");
    tracing::info!("running fleet-db migrations…");
    migrate(&pool).await?;
    Ok(pool)
}

/// Map sqlx errors with unique-violation → [`DbError::Conflict`].
/// Logs the failure so query errors are never silent.
pub fn map_query_err(err: sqlx::Error) -> DbError {
    DbError::from_sqlx(err)
}
