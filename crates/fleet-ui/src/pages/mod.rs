//! Feature-based pages.
//!
//! Each subdirectory is one feature. URL routes are declared in `main.rs`
//! (`/{workspace_id}/dashboard`, etc.) — folders are only for code organization.
//!
//! ```text
//! pages/
//!   dashboard/     → route /{workspace_id}/dashboard
//!     mod.rs
//!     page.rs      → UI component
//!     api.rs       → client fetch helpers for this page
//!   sessions/      → route /{workspace_id}/sessions
//!   …
//!   shared/        → EmptyPage and other cross-page UI
//! ```

pub mod agents;
pub mod auth;
pub mod cases;
pub mod chat;
pub mod dashboard;
pub mod fleet;
pub mod home;
pub mod integrations;
pub mod sandboxes;
pub mod sessions;
pub mod settings;
pub mod shared;
