//! Step 13 — orphan sandbox reaping.

use crate::sandbox::{RuntimeHandle, SandboxRuntime};
use crate::store::{BoardSnapshot, Store};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use super::tick::TickError;

pub async fn orphan_reap(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    project_id: Uuid,
    snap: &BoardSnapshot,
) -> Result<usize, TickError> {
    let known: HashSet<String> = snap
        .sessions
        .iter()
        .filter_map(|s| s.runtime_handle.clone())
        .collect();

    let listed = runtime.list(project_id).await.map_err(|e| {
        TickError::Other(format!("runtime list: {e}"))
    })?;

    let mut destroyed = 0usize;
    for h in listed {
        if !known.contains(&h.id) {
            let _ = runtime.destroy(&h).await;
            destroyed += 1;
        }
    }

    // Terminal tasks still claiming sessions
    for t in &snap.tasks {
        if matches!(t.status.as_str(), "done_success" | "done_failed" | "cancelled") {
            if let Some(sid) = t.claimed_session_id {
                if let Some(s) = store.get_session(sid).await? {
                    if let Some(hid) = s.runtime_handle {
                        let h = RuntimeHandle {
                            backend: s.runtime_backend,
                            id: hid,
                        };
                        let _ = runtime.destroy(&h).await;
                    }
                    store
                        .free_session_claim(t.id, sid, "clean")
                        .await?;
                    destroyed += 1;
                }
            }
        }
    }
    Ok(destroyed)
}
