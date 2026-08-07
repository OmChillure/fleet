//! Discord control-plane tools (`client` + `tools`). User OAuth: identify + guilds.

mod client;
pub mod tools;

pub use tools::{invoke, OPERATIONS};
