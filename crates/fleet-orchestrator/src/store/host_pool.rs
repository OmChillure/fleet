//! Paired worker-host pool for tick spawn.
//!
//! Uses `fleet_db::repos::worker_hosts::list_assignable`. Missing table /
//! query error → no host (local `SANDBOX_BACKEND` still works).

use super::Store;
use crate::sandbox::{SpawnRequest, remote};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignableHost {
    pub id: Uuid,
    pub allow_host: bool,
    pub allow_container: bool,
    pub advertised_host: bool,
    pub advertised_container: bool,
    pub max_concurrent: i32,
    pub advertised_max_concurrent: Option<i32>,
    pub max_job_timeout_s: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRuntime {
    Host,
    Container,
}

impl HostRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Container => "container",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenHost {
    pub host: AssignableHost,
    pub runtime: HostRuntime,
}

impl ChosenHost {
    pub fn id(&self) -> Uuid {
        self.host.id
    }
}

/// v1 is host-only. Container / k3d is not a working executor — do not
/// assign `runtime=container` even when the plane and the box advertise it.
pub fn effective_runtime(host: &AssignableHost) -> Option<HostRuntime> {
    if host.allow_host && host.advertised_host {
        Some(HostRuntime::Host)
    } else {
        None
    }
}

fn cap(host: &AssignableHost) -> i32 {
    let cp = host.max_concurrent.clamp(1, 256);
    match host.advertised_max_concurrent {
        Some(n) => cp.min(n.clamp(1, 256)),
        None => cp,
    }
}

pub fn host_at_capacity(in_flight: i64, host: &AssignableHost) -> bool {
    in_flight >= i64::from(cap(host))
}

pub fn job_timeout_s(task_timeout_s: u64, max_job_timeout_s: i32) -> i32 {
    let task = i32::try_from(task_timeout_s).unwrap_or(i32::MAX).max(1);
    task.min(max_job_timeout_s.max(60))
}

pub fn pick_assignable_host(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
) -> Option<ChosenHost> {
    for host in hosts {
        let Some(runtime) = effective_runtime(host) else {
            continue;
        };
        let n = in_flight.get(&host.id).copied().unwrap_or(0);
        if host_at_capacity(n, host) {
            continue;
        }
        return Some(ChosenHost {
            host: host.clone(),
            runtime,
        });
    }
    None
}

/// Before `begin_spawn_assignment`: first host that is online, effective,
/// and under `min(max_concurrent, advertised_max_concurrent, 256)`.
pub async fn select_assignable_host(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    let hosts = list_assignable_hosts(store, project_id).await?;
    if hosts.is_empty() {
        return None;
    }
    let in_flight = count_remote_in_flight(store, project_id).await?;
    pick_assignable_host(&hosts, &in_flight)
}

pub async fn enqueue_remote_spawn(
    store: &Store,
    chosen: &ChosenHost,
    req: &SpawnRequest,
    orchestrator_url: &str,
) -> Result<(), String> {
    let timeout_s = job_timeout_s(req.timeout.as_secs(), chosen.host.max_job_timeout_s);
    let payload = remote::spawn_payload(
        req,
        orchestrator_url,
        chosen.runtime.as_str(),
        timeout_s,
        chosen.id(),
    );
    let value = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
    let claim = fleet_db::NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id: req.project_id,
        kind: "spawn_session".into(),
        payload: value,
        max_attempts: 3,
        available_at: chrono::Utc::now(),
    };
    fleet_db::repos::work_claims::enqueue(store.pool(), &claim)
        .await
        .map_err(|e| e.to_string())
}

impl From<&fleet_db::WorkerHost> for AssignableHost {
    fn from(h: &fleet_db::WorkerHost) -> Self {
        Self {
            id: h.id,
            allow_host: h.allow_host,
            allow_container: h.allow_container,
            advertised_host: h.advertised_host,
            advertised_container: h.advertised_container,
            max_concurrent: h.max_concurrent,
            advertised_max_concurrent: h.advertised_max_concurrent,
            max_job_timeout_s: h.max_job_timeout_s,
        }
    }
}

async fn list_assignable_hosts(store: &Store, project_id: Uuid) -> Option<Vec<AssignableHost>> {
    match fleet_db::repos::worker_hosts::list_assignable(
        store.pool(),
        project_id,
        chrono::Duration::seconds(30),
    )
    .await
    {
        Ok(hosts) => Some(hosts.iter().map(AssignableHost::from).collect()),
        Err(e) => {
            tracing::debug!(
                error = %e,
                project_id = %project_id,
                "worker_hosts unavailable; using local sandbox"
            );
            None
        }
    }
}

async fn count_remote_in_flight(store: &Store, project_id: Uuid) -> Option<HashMap<Uuid, i64>> {
    let rows = sqlx::query_as::<_, (Option<String>, i64)>(
        r#"
        SELECT runtime_handle, COUNT(*)::bigint
        FROM worker_sessions
        WHERE project_id = $1
          AND runtime_backend = 'remote'
          AND status IN ('spawning', 'healthy')
        GROUP BY runtime_handle
        "#,
    )
    .bind(project_id)
    .fetch_all(store.pool())
    .await
    .ok()?;
    let mut map = HashMap::new();
    for (handle, n) in rows {
        let Some(handle) = handle else {
            continue;
        };
        if let Some(id) = remote::host_id_from_handle(&handle) {
            *map.entry(id).or_insert(0) += n;
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(
        id: Uuid,
        allow_h: bool,
        allow_c: bool,
        adv_h: bool,
        adv_c: bool,
        max: i32,
    ) -> AssignableHost {
        AssignableHost {
            id,
            allow_host: allow_h,
            allow_container: allow_c,
            advertised_host: adv_h,
            advertised_container: adv_c,
            max_concurrent: max,
            advertised_max_concurrent: None,
            max_job_timeout_s: 3600,
        }
    }

    #[test]
    fn v1_assigns_host_even_when_container_is_advertised() {
        let h = host(Uuid::new_v4(), true, true, true, true, 4);
        assert_eq!(effective_runtime(&h), Some(HostRuntime::Host));
    }

    #[test]
    fn v1_skips_container_only_host() {
        let h = host(Uuid::new_v4(), true, true, false, true, 4);
        assert_eq!(effective_runtime(&h), None);
        assert!(pick_assignable_host(&[h], &HashMap::new()).is_none());
    }

    #[test]
    fn host_when_container_not_advertised() {
        let h = host(Uuid::new_v4(), true, true, true, false, 4);
        assert_eq!(effective_runtime(&h), Some(HostRuntime::Host));
    }

    #[test]
    fn skip_when_plane_disallows_both() {
        let h = host(Uuid::new_v4(), false, false, true, true, 4);
        assert_eq!(effective_runtime(&h), None);
    }

    #[test]
    fn none_when_host_allowed_but_not_advertised_and_no_container() {
        let h = host(Uuid::new_v4(), true, false, false, false, 4);
        assert_eq!(effective_runtime(&h), None);
    }

    #[test]
    fn skip_at_max_concurrent_and_256_cap() {
        let at_cp = host(Uuid::new_v4(), true, true, true, true, 4);
        assert!(host_at_capacity(4, &at_cp));
        assert!(!host_at_capacity(3, &at_cp));
        let over = host(Uuid::new_v4(), true, true, true, true, 999);
        assert!(host_at_capacity(256, &over));
        assert!(!host_at_capacity(255, &over));
    }

    #[test]
    fn skip_when_advertised_cap_reached() {
        let id = Uuid::new_v4();
        let mut h = host(id, true, true, true, true, 8);
        h.advertised_max_concurrent = Some(2);
        assert!(host_at_capacity(2, &h));
        assert!(!host_at_capacity(1, &h));
        let mut inflight = HashMap::new();
        inflight.insert(id, 2);
        assert!(pick_assignable_host(&[h], &inflight).is_none());
    }

    #[test]
    fn pick_skips_full_and_ineffective() {
        let full_id = Uuid::new_v4();
        let none_id = Uuid::new_v4();
        let ok_id = Uuid::new_v4();
        let hosts = vec![
            host(full_id, true, true, true, true, 1),
            host(none_id, false, false, true, true, 4),
            host(ok_id, true, false, true, false, 2),
        ];
        let mut inflight = HashMap::new();
        inflight.insert(full_id, 1);
        let chosen = pick_assignable_host(&hosts, &inflight).unwrap();
        assert_eq!(chosen.id(), ok_id);
        assert_eq!(chosen.runtime, HostRuntime::Host);
    }

    #[test]
    fn pick_none_when_all_full() {
        let id = Uuid::new_v4();
        let hosts = vec![host(id, true, true, true, true, 2)];
        let mut inflight = HashMap::new();
        inflight.insert(id, 2);
        assert!(pick_assignable_host(&hosts, &inflight).is_none());
    }

    #[test]
    fn timeout_is_min_task_and_host_cap() {
        assert_eq!(job_timeout_s(7200, 3600), 3600);
        assert_eq!(job_timeout_s(30, 3600), 30);
        assert_eq!(job_timeout_s(10, 10), 10);
    }
}
