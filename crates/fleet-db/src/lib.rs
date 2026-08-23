//! # fleet-db
//!
//! Shared Postgres layer for Agent Fleet services (`fleet-auth`, orchestrator, …).
//!
//! | Module | Role |
//! | --- | --- |
//! | [`pool`] | Connect + migrate (`DATABASE_URL`) |
//! | [`models`] | Row types (`User`, `AuthSession`, `WorkClaim`, …) |
//! | [`repos`] | Query helpers (no HTTP / JWT knowledge) |
//! | `migrations/` | Schema (embedded via `sqlx::migrate!`) |
//!
//! Live tables are listed in `README.md`. Unused speculative tables
//! (`audit_events`, orchestrator `project_leases` / `idempotency_keys` /
//! `orchestration_events`) are dropped in `020_drop_unused.sql`.

#![forbid(unsafe_code)]

pub mod error;
pub mod models;
pub mod pool;
pub mod repos;

pub use error::{DbError, DbResult};
pub use models::*;
pub use pool::{
    PoolConfig, connect, connect_and_migrate, connect_and_migrate_with, migrate,
    normalize_database_url,
};
pub use sqlx::{PgPool, Postgres, Transaction};
