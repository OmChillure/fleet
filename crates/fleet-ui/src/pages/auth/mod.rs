//! Auth feature: login today, signup later.
//!
//! URLs: `/login` (and `/signup` when added). Folder is `auth/`, not the path.

mod api;
mod login;

pub use login::Login;
