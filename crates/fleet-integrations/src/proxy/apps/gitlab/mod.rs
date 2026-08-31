//! GitLab control-plane tools (`client` + `tools`).

mod client;
pub mod tools;

pub use tools::{OPERATIONS, invoke};
