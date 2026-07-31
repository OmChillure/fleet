//! Client-side API helpers for the dashboard feature.
//!
//! Backend HTTP routes are separate (e.g. `GET /api/…`).
//! This module is only "how this page talks to the backend."

#![allow(dead_code)]

/// Placeholder types / fetch functions for dashboard data.
/// Example later:
/// ```ignore
/// pub async fn fetch_overview() -> Result<Overview, ApiError> { … }
/// ```
pub struct Overview {
    pub active_agents: u32,
    pub open_sessions: u32,
}
