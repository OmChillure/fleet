//! Dashboard feature (`/{workspace_id}/dashboard`).
//!
//! Keep page UI, client API helpers, and local types together here.
//! Route path stays `/dashboard` — only the source layout is folder-based.

mod api;
mod page;

pub use page::Dashboard;
