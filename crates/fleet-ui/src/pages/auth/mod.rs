//! Auth feature: login + signup (JWT cookies via fleet-auth).
//!
//! URLs: `/login`, `/signup`.

pub mod api;
mod layout;
mod login;
mod signup;

pub use login::Login;
pub use signup::Signup;
