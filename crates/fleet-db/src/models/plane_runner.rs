//! Workspace control-plane runner (always first on the Self-hosted list).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    WORKER_HOST_ISOLATION_NATIVE, WORKER_HOST_KIND_CONTROL_PLANE, WORKER_SOFTWARE_STATUS_UNKNOWN,
    WorkerHostPublic,
};

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

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PlaneRunner {
    pub workspace_id: Uuid,
    pub name: String,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub updated_at: DateTime<Utc>,
}

impl PlaneRunner {
    pub fn default_for(workspace_id: Uuid) -> Self {
        Self {
            workspace_id,
            name: "Control plane".into(),
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            updated_at: Utc::now(),
        }
    }

    pub fn taking_native_jobs(&self) -> bool {
        self.allow_host
    }

    pub fn as_public(&self, now: DateTime<Utc>) -> WorkerHostPublic {
        self.as_public_with_jobs(now, 0)
    }

    pub fn as_public_with_jobs(&self, now: DateTime<Utc>, current_jobs: i32) -> WorkerHostPublic {
        let taking = self.allow_host || self.allow_container;
        WorkerHostPublic {
            id: control_plane_runner_id(self.workspace_id),
            workspace_id: self.workspace_id,
            name: {
                let n = self.name.trim();
                if n.is_empty() {
                    "Control plane".into()
                } else {
                    n.to_string()
                }
            },
            kind: WORKER_HOST_KIND_CONTROL_PLANE.to_string(),
            status: "online".into(),
            enabled: taking,
            paused: false,
            drain: !taking,
            isolation: WORKER_HOST_ISOLATION_NATIVE.to_string(),
            allow_host: self.allow_host,
            allow_container: self.allow_container,
            max_concurrent: self.max_concurrent.clamp(1, 256),
            max_job_timeout_s: 3600,
            labels: vec!["control-plane".into()],
            advertised_host: true,
            advertised_container: false,
            platform: Some(local_platform()),
            hostname: Some("control plane".into()),
            last_seen_at: Some(now),
            last_error: None,
            created_at: self.updated_at,
            paired_at: Some(self.updated_at),
            software: None,
            desired: None,
            software_status: WORKER_SOFTWARE_STATUS_UNKNOWN.to_string(),
            outdated: false,
            auto_update: false,
            update_requested_at: None,
            machine_id: None,
            settings_gen: 0,
            settings_ack_gen: 0,
            settings_pending: false,
            current_jobs: current_jobs.max(0),
            advertised_max_concurrent: None,
            hardware: None,
        }
    }
}

fn local_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => "linux-x86_64".into(),
        ("linux", "aarch64") => "linux-aarch64".into(),
        ("macos", "x86_64") => "darwin-x86_64".into(),
        ("macos", "aarch64") => "darwin-aarch64".into(),
        ("windows", "x86_64") => "windows-x86_64".into(),
        ("windows", "aarch64") => "windows-arm64".into(),
        _ => format!("{os}-{arch}"),
    }
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

    #[test]
    fn default_takes_native_jobs() {
        let row = PlaneRunner::default_for(Uuid::now_v7());
        assert!(row.taking_native_jobs());
        let mut off = row.clone();
        off.allow_host = false;
        assert!(!off.taking_native_jobs());
    }

    #[test]
    fn public_view_is_control_plane_and_online() {
        let ws = Uuid::now_v7();
        let row = PlaneRunner::default_for(ws);
        let pub_ = row.as_public(Utc::now());
        assert_eq!(pub_.kind, WORKER_HOST_KIND_CONTROL_PLANE);
        assert_eq!(pub_.id, control_plane_runner_id(ws));
        assert_eq!(pub_.status, "online");
        assert!(!pub_.settings_pending);
        assert_eq!(pub_.name, "Control plane");
        assert!(pub_.software.is_none());
        assert_eq!(pub_.software_status, WORKER_SOFTWARE_STATUS_UNKNOWN);
        assert_eq!(pub_.current_jobs, 0);
        assert_eq!(row.as_public_with_jobs(Utc::now(), 2).current_jobs, 2);
    }
}
