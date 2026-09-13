//! Auth domain for Agent Fleet — routes, JWT cookies, password hashing.
//!
//! Mounted by [`fleet-server`](../fleet-server). Does not own `main`.

// `std::env::set_var` is unsafe on recent Rust; tests may mutate process env.
#![cfg_attr(not(test), forbid(unsafe_code))]

mod cookies;
mod error;
mod models;
mod password;
mod rate_limit;
mod routes;
mod tokens;

pub mod config;
pub mod state;

pub use config::{
    AuthConfig, public_url_requires_signup_token, validate_signup_policy,
};
pub use cookies::validate_cookie_policy;
pub use error::{AppError, AppResult};
pub use routes::{require_instance_admin, require_user, router};
pub use state::AuthState;
pub use tokens::{
    ACCESS_COOKIE, AccessClaims, REFRESH_COOKIE, decode_access_token, issue_access_token,
};
