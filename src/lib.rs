//! Agent Fleet orchestration service library.
//!
//! Pure-logic control plane after a `WorkPlan` is accepted.
//! Stack: Rust + PostgreSQL 16 + abstract `SandboxRuntime`.

pub mod config;
pub mod domain;
pub mod notify;
pub mod sandbox;
pub mod store;
pub mod sync;
pub mod validate;
