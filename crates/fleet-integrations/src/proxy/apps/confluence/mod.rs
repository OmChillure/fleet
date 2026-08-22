//! Confluence Cloud control-plane tools. OAuth may still be “coming soon”;
//! ops are live so `integration_query` works the moment a connection exists.

mod client;
pub mod tools;

pub use tools::{invoke, OPERATIONS};
