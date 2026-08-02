//! Auth domain for Agent Fleet — routes, JWT cookies, password hashing.
//!
//! Mounted by [`fleet-server`](../fleet-server). Does not own `main`.

#![forbid(unsafe_code)]

mod cookies;
mod error;
mod models;
mod password;
mod routes;
mod tokens;

pub mod config;
pub mod state;

pub use config::AuthConfig;
pub use cookies::validate_cookie_policy;
pub use error::{AppError, AppResult};
pub use routes::{require_user, router};
pub use state::AuthState;
pub use tokens::{ACCESS_COOKIE, REFRESH_COOKIE};
