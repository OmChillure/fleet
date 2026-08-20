//! Paired worker-host pool for tick spawn.
//!
//! Uses `fleet_db::repos::worker_hosts::list_assignable`. Missing table /
//! query error → no host (local `SANDBOX_BACKEND` still works).
//! Cloud / Partners / public-cloud are not assignable and are not a
//! second spawn path.

use super::Store;
use crate::sandbox::{SpawnRequest, remote};
use std::collections::HashMap;
use uuid::Uuid;

/// Isolation of a runner. Only Native is assignable.
/// Copied from `worker_hosts.isolation` (`native` / `sandboxed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerIsolation {
    #[default]
    Native,
    Sandboxed,
}

impl RunnerIsolation {
    pub fn is_assignable(self) -> bool {
        matches!(self, Self::Native)
    }
}

/// Product execution kind. Only self-hosted native is assignable.
/// Cloud / Partners / public-cloud stay off the picker (no k8s executor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    SelfHostedNative,
    SelfHostedSandbox,
    PublicCloud,
    FleetCloud,
    Partners,
}

impl RunnerKind {
    pub fn is_assignable(self) -> bool {
        matches!(self, Self::SelfHostedNative)
    }

    pub fn isolation(self) -> RunnerIsolation {
        match self {
            Self::SelfHostedNative => RunnerIsolation::Native,
            Self::SelfHostedSandbox | Self::PublicCloud | Self::FleetCloud | Self::Partners => {
                RunnerIsolation::Sandboxed
            }
        }
    }
}

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
    pub isolation: RunnerIsolation,
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

/// Today's host picker, then Native-only. No second list/query.
/// Cloud / Partners / public-cloud cannot be chosen.
pub fn pick_assignable_runner(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
) -> Option<ChosenHost> {
    let native: Vec<AssignableHost> = hosts
        .iter()
        .filter(|h| h.isolation.is_assignable())
        .cloned()
        .collect();
    pick_assignable_host(&native, in_flight)
}

/// Wraps [`select_assignable_host`]. Tick fallback is unchanged.
/// Cloud / Partners / public-cloud cannot be chosen.
pub async fn select_assignable_runner(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    select_assignable_host(store, project_id)
        .await
        .filter(|chosen| chosen.host.isolation.is_assignable())
}

/// How long after last_seen a `software_updating` host still holds ready tasks.
pub const HOST_UPDATE_HOLD_SECS: i64 = 120;

/// Paired host is mid host-bin restart: do not fail over to local sandbox.
pub async fn workspace_has_updating_host(store: &Store, project_id: Uuid) -> bool {
    match fleet_db::repos::worker_hosts::list_updating_recent(
        store.pool(),
        project_id,
        chrono::Duration::seconds(HOST_UPDATE_HOLD_SECS),
    )
    .await
    {
        Ok(hosts) => !hosts.is_empty(),
        Err(e) => {
            tracing::debug!(
                error = %e,
                project_id = %project_id,
                "worker_hosts updating query unavailable"
            );
            false
        }
    }
}

/// No assignable host + a recent updating host → leave the task ready.
pub fn hold_ready_for_host_update(has_assignable: bool, updating_recent: bool) -> bool {
    !has_assignable && updating_recent
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
            isolation: match h.isolation.as_str() {
                fleet_db::WORKER_HOST_ISOLATION_SANDBOXED => RunnerIsolation::Sandboxed,
                _ => RunnerIsolation::Native,
            },
        }
    }
}

async fn list_assignable_hosts(store: &Store, project_id: Uuid) -> Option<Vec<AssignableHost>> {
    match fleet_db::repos::worker_hosts::list_assignable(
        store.pool(),
        project_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
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
            isolation: RunnerIsolation::Native,
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

    #[test]
    fn hold_ready_only_when_no_assignable_and_updating() {
        assert!(hold_ready_for_host_update(false, true));
        assert!(!hold_ready_for_host_update(true, true));
        assert!(!hold_ready_for_host_update(false, false));
        assert!(!hold_ready_for_host_update(true, false));
    }

    #[test]
    fn runner_default_is_native_and_picks_paired_host() {
        let h = host(Uuid::new_v4(), true, true, true, true, 4);
        assert_eq!(h.isolation, RunnerIsolation::Native);
        let chosen = pick_assignable_runner(&[h.clone()], &HashMap::new()).unwrap();
        assert_eq!(chosen.id(), h.id);
        assert_eq!(chosen.runtime, HostRuntime::Host);
        assert_eq!(
            pick_assignable_runner(&[h.clone()], &HashMap::new()),
            pick_assignable_host(&[h], &HashMap::new())
        );
    }

    #[test]
    fn runner_skips_sandboxed_and_picks_native() {
        let native_id = Uuid::new_v4();
        let mut sandboxed = host(Uuid::new_v4(), true, true, true, true, 4);
        sandboxed.isolation = RunnerIsolation::Sandboxed;
        let native = host(native_id, true, true, true, true, 4);
        assert!(pick_assignable_runner(&[sandboxed.clone()], &HashMap::new()).is_none());
        let chosen = pick_assignable_runner(&[sandboxed, native], &HashMap::new()).unwrap();
        assert_eq!(chosen.id(), native_id);
        assert_eq!(chosen.host.isolation, RunnerIsolation::Native);
    }

    fn worker_host(isolation: &str) -> fleet_db::WorkerHost {
        let now = chrono::Utc::now();
        fleet_db::WorkerHost {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            name: "box".into(),
            status: fleet_db::WORKER_HOST_STATUS_ONLINE.into(),
            enabled: true,
            paused: false,
            drain: false,
            isolation: isolation.into(),
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: None,
            advertised_host: true,
            advertised_container: false,
            platform: None,
            hostname: None,
            last_seen_at: Some(now),
            last_error: None,
            current_jobs: 0,
            advertised_max_concurrent: None,
            software_git_sha: None,
            software_bins: None,
            software_reported_at: None,
            software_updating: false,
            auto_update: true,
            update_requested_at: None,
            machine_id: None,
            hardware: None,
            created_by: None,
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn from_worker_host_maps_isolation() {
        let native = AssignableHost::from(&worker_host(fleet_db::WORKER_HOST_ISOLATION_NATIVE));
        assert_eq!(native.isolation, RunnerIsolation::Native);
        let boxed = AssignableHost::from(&worker_host(fleet_db::WORKER_HOST_ISOLATION_SANDBOXED));
        assert_eq!(boxed.isolation, RunnerIsolation::Sandboxed);
        let unknown = AssignableHost::from(&worker_host("k8s"));
        assert_eq!(unknown.isolation, RunnerIsolation::Native);
    }
}
