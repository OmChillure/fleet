//! Step 13 — free claims on terminal tasks.

use crate::store::{BoardSnapshot, Store};
use uuid::Uuid;

use super::tick::TickError;

pub async fn orphan_reap(
    store: &Store,
    _project_id: Uuid,
    snap: &BoardSnapshot,
) -> Result<usize, TickError> {
    let mut freed = 0usize;
    for t in &snap.tasks {
        if matches!(
            t.status.as_str(),
            "done_success" | "done_failed" | "cancelled"
        ) && let Some(sid) = t.claimed_session_id
        {
            store.free_session_claim(t.id, sid, "clean").await?;
            freed += 1;
        }
    }
    Ok(freed)
}
