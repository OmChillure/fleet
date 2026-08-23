//! Confluence Cloud control-plane tools. OAuth connect is live;
//! `integration_query` works the moment a connection exists.

mod client;
pub mod tools;

pub use tools::{invoke, OPERATIONS};
