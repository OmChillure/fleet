use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

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
        // Neon free-tier / cold start often needs >10s for TCP+TLS+auth.
        // Override with DATABASE_ACQUIRE_TIMEOUT_SECS (default 60).
        let acquire_secs = std::env::var("DATABASE_ACQUIRE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(5, 300);
        Self {
            max_connections: 20,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(acquire_secs),
            idle_timeout: Some(Duration::from_secs(600)),
            max_lifetime: Some(Duration::from_secs(1800)),
        }
    }
}

/// Connect without running migrations.
pub async fn connect(database_url: &str) -> DbResult<PgPool> {
    connect_with(database_url, &PoolConfig::default()).await
}

/// Hosted URL tweaks for sqlx:
/// rewrite Supabase transaction pooler (`:6543`) to session (`:5432`) and
/// add `sslmode=require` when missing.
pub fn normalize_database_url(database_url: &str) -> String {
    let mut url = database_url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if lower.contains("pooler.supabase") && lower.contains(":6543") {
        url = url.replace(":6543", ":5432");
    }
    let lower = url.to_ascii_lowercase();
    if !lower.contains("sslmode=") && is_hosted_postgres(&lower) {
        url = if url.contains('?') {
            format!("{url}&sslmode=require")
        } else {
            format!("{url}?sslmode=require")
        };
    }
    url
}

fn is_hosted_postgres(url_lower: &str) -> bool {
    url_lower.contains("neon.tech")
        || url_lower.contains("supabase.co")
        || url_lower.contains("supabase.com")
        || url_lower.contains("pooler.supabase")
}

fn disable_statement_cache(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains(":6543")
        || lower.contains("pooler.supabase")
        || lower.contains("-pooler.")
        || lower.contains("pgbouncer=true")
        || lower.contains("supabase.co")
        || lower.contains("supabase.com")
}

fn connect_options(database_url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    let mut opts = PgConnectOptions::from_str(database_url)?;
    if disable_statement_cache(database_url) {
        opts = opts.statement_cache_capacity(0);
    }
    Ok(opts)
}

pub async fn connect_with(database_url: &str, cfg: &PoolConfig) -> DbResult<PgPool> {
    let database_url = normalize_database_url(database_url);
    let connect_opts = match connect_options(&database_url) {
        Ok(o) => o,
        Err(err) => {
            let target = redact_db_target(&database_url);
            return Err(DbError::Connect {
                target,
                source: err,
            });
        }
    };

    let mut opts = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .acquire_timeout(cfg.acquire_timeout)
        .after_release(|conn, _meta| {
            Box::pin(async move {
                let _ = sqlx::query("SELECT pg_advisory_unlock_all()")
                    .execute(&mut *conn)
                    .await;
                Ok(true)
            })
        });

    if let Some(idle) = cfg.idle_timeout {
        opts = opts.idle_timeout(idle);
    }
    if let Some(life) = cfg.max_lifetime {
        opts = opts.max_lifetime(life);
    }

    match opts.connect_with(connect_opts).await {
        Ok(pool) => Ok(pool),
        Err(err) => {
            let target = redact_db_target(&database_url);
            tracing::error!(
                target = %target,
                error = %err,
                acquire_timeout_s = cfg.acquire_timeout.as_secs(),
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
    let without_scheme = database_url.split("://").nth(1).unwrap_or(database_url);
    let after_at = without_scheme.split('@').nth(1).unwrap_or(without_scheme);
    let host_and_path = after_at.split('?').next().unwrap_or(after_at);
    host_and_path.to_string()
}

/// Apply embedded migrations from this crate (`migrations/`).
///
/// `ignore_missing`: other crates (e.g. fleet-orchestrator) may share the same
/// `_sqlx_migrations` table with additional version rows.
pub async fn migrate(pool: &PgPool) -> DbResult<()> {
    // One connection for the whole migrator; clear leftovers from a recycled
    // pooler backend (defensive against "sqlx_s_1 already exists").
    let mut conn = pool.acquire().await.map_err(DbError::from_sqlx)?;
    let _ = sqlx::query("DEALLOCATE ALL").execute(&mut *conn).await;
    match sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(&mut *conn)
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

pub async fn connect_and_migrate_with(database_url: &str, cfg: &PoolConfig) -> DbResult<PgPool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supabase_transaction_pooler_becomes_session() {
        let out = normalize_database_url(
            "postgresql://postgres.abc:pw@aws-0-us-east-1.pooler.supabase.com:6543/postgres",
        );
        assert!(out.contains(":5432/"), "{out}");
        assert!(!out.contains(":6543"));
        assert!(out.contains("sslmode=require"), "{out}");
    }

    #[test]
    fn hosted_url_keeps_existing_sslmode() {
        let out = normalize_database_url(
            "postgresql://u:p@db.abc.supabase.co:5432/postgres?sslmode=verify-full",
        );
        assert!(out.contains("sslmode=verify-full"), "{out}");
        assert!(!out.contains("sslmode=require"));
    }

    #[test]
    fn local_url_untouched() {
        let src = "postgres://postgres:postgres@127.0.0.1:5432/agent_fleet";
        assert_eq!(normalize_database_url(src), src);
    }

    #[test]
    fn neon_gets_sslmode() {
        let out = normalize_database_url("postgres://u:p@ep-x.us-east-1.aws.neon.tech/neondb");
        assert!(out.ends_with("?sslmode=require"), "{out}");
    }
}
