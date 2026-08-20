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
//! Schema is designed for high concurrency (UUID PKs, SKIP LOCKED claims,
//! single-row session revoke). See migration comments.

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
