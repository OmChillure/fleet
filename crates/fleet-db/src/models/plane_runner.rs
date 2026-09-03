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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_id_is_stable_per_workspace() {
        let ws = Uuid::parse_str("01a01c1f-d8d0-7e03-8421-2d83002d9cb9").expect("ws");
        let a = control_plane_runner_id(ws);
        let b = control_plane_runner_id(ws);
        assert_eq!(a, b);
        assert_ne!(a, ws);
        assert!(is_control_plane_runner_id(ws, a));
        assert!(!is_control_plane_runner_id(ws, Uuid::now_v7()));
    }
}
