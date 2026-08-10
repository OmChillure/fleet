//! Google Drive control-plane tools (`client` + `tools`). Read-only OAuth scope.

mod client;
pub mod tools;

pub use tools::{invoke, OPERATIONS};
