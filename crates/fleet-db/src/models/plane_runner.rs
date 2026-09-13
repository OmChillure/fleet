//! Stable id so the API process is never mistaken for a paired host.

use uuid::Uuid;

const PLANE_ID_NS: &str = "agent-fleet:control-plane:";

/// Stable per-workspace id. Never a paired `worker_hosts` v7.
pub fn control_plane_runner_id(workspace_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{PLANE_ID_NS}{workspace_id}").as_bytes(),
    )
}

pub fn is_control_plane_runner_id(workspace_id: Uuid, host_id: Uuid) -> bool {
    host_id == control_plane_runner_id(workspace_id)
}
