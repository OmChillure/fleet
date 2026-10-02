//! Shared helpers for fleet-db Postgres integration tests.
//! Fail-closed: missing DATABASE_URL panics (never skip).

use std::time::Duration;

use fleet_db::{PgPool, PoolConfig, connect_and_migrate_with};
use uuid::Uuid;

pub fn require_database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "DATABASE_URL is required for this integration test. \
                 Start Postgres (just db-up) and export DATABASE_URL."
            )
        })
}

pub async fn connect_and_migrate() -> PgPool {
    let url = require_database_url();
    connect_and_migrate_with(
        &url,
        &PoolConfig {
            max_connections: 5,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(30)),
            max_lifetime: None,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("connect_and_migrate failed: {e}"))
}

pub fn unique_email() -> String {
    let uuid = Uuid::new_v4();
    format!("db.{uuid}@example.test")
}

pub fn unique_slug(prefix: &str) -> String {
    let uuid = Uuid::new_v4();
    format!("{prefix}-{}", &uuid.to_string()[..8])
}
