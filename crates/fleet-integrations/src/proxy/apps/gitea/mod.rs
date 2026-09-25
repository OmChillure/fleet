//! Gitea control-plane tools (`client` + `tools`).
//!
//! Self-hosted: the connection config carries `base_url` (no trailing slash).

mod client;
pub mod tools;

pub use tools::{invoke, OPERATIONS};
