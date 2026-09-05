//! Paired worker-host pool for tick spawn.
//!
//! Uses `fleet_db::repos::worker_hosts::list_assignable`. Missing table /
//! query error → no host (task stays ready unless a test enables local spawn).
//! Cloud / Partners / public-cloud are not assignable and are not a
//! second spawn path.
//!
//! Isolation (`process` < `container` < `vm`) is not placement. A
//! container-only in-cluster host is assignable when it meets workspace
//! `min_isolation`.

use super::Store;
use crate::domain::{pick_isolation, IsolationClass};
use crate::sandbox::{remote, SpawnRequest};
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
    /// Oldest heartbeat wins when load/cap is tied. List order is newest-first.
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Home workspace. On a load tie, a home host beats a cluster host.
    pub workspace_id: Uuid,
    /// Last advertised hostname. Loopback plane URLs may only assign this machine.
    pub hostname: Option<String>,
    /// Non-loopback IPs from the last heartbeat (`hardware.addrs`).
    pub addrs: Vec<String>,
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

/// Isolation classes this host can run (advertised ∩ plane allow).
pub fn offered_classes(host: &AssignableHost) -> Vec<IsolationClass> {
    let mut offer = Vec::new();
    if host.allow_host && host.advertised_host {
        offer.push(IsolationClass::Process);
    }
    if host.allow_container && host.advertised_container {
        offer.push(IsolationClass::Container);
    }
    offer
}

pub fn runtime_for_class(class: IsolationClass) -> Option<HostRuntime> {
    match class {
        IsolationClass::Process => Some(HostRuntime::Host),
        IsolationClass::Container => Some(HostRuntime::Container),
        IsolationClass::Vm => None,
    }
}

/// Weakest offered class that still meets `need` (default `process`).
pub fn effective_runtime_for(host: &AssignableHost, need: IsolationClass) -> Option<HostRuntime> {
    let class = pick_isolation(need, &offered_classes(host))?;
    runtime_for_class(class)
}

/// Default need is `process` (workspace `min_isolation` default).
pub fn effective_runtime(host: &AssignableHost) -> Option<HostRuntime> {
    effective_runtime_for(host, IsolationClass::Process)
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

pub fn pick_assignable_host_for(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
    need: IsolationClass,
) -> Option<ChosenHost> {
    pick_assignable_host_in(hosts, in_flight, need, None)
}

fn pick_assignable_host_in(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
    need: IsolationClass,
    prefer_workspace: Option<Uuid>,
) -> Option<ChosenHost> {
    let mut best: Option<(
        ChosenHost,
        i64,
        i32,
        bool,
        Option<chrono::DateTime<chrono::Utc>>,
        usize,
    )> = None;
    for (idx, host) in hosts.iter().enumerate() {
        let Some(runtime) = effective_runtime_for(host, need) else {
            continue;
        };
        let n = in_flight.get(&host.id).copied().unwrap_or(0);
        if host_at_capacity(n, host) {
            continue;
        }
        let cap = cap(host);
        let home = prefer_workspace == Some(host.workspace_id);
        let better = match &best {
            None => true,
            Some((_, bn, bcap, bhome, bseen, bidx)) => load_then_home_then_oldest(
                n,
                cap,
                home,
                host.last_seen_at,
                idx,
                *bn,
                *bcap,
                *bhome,
                *bseen,
                *bidx,
            ),
        };
        if better {
            best = Some((
                ChosenHost {
                    host: host.clone(),
                    runtime,
                },
                n,
                cap,
                home,
                host.last_seen_at,
                idx,
            ));
        }
    }
    best.map(|(chosen, _, _, _, _, _)| chosen)
}

/// Lower load/cap, then home workspace, then older last_seen, then last in newest-first list.
fn load_then_home_then_oldest(
    n: i64,
    cap: i32,
    home: bool,
    seen: Option<chrono::DateTime<chrono::Utc>>,
    idx: usize,
    other_n: i64,
    other_cap: i32,
    other_home: bool,
    other_seen: Option<chrono::DateTime<chrono::Utc>>,
    other_idx: usize,
) -> bool {
    let left = n.saturating_mul(i64::from(other_cap.max(1)));
    let right = other_n.saturating_mul(i64::from(cap.max(1)));
    if left != right {
        return left < right;
    }
    match (home, other_home) {
        (true, false) => return true,
        (false, true) => return false,
        _ => {}
    }
    match (seen, other_seen) {
        (Some(a), Some(b)) if a != b => return a < b,
        (Some(_), None) => return true,
        (None, Some(_)) => return false,
        _ => {}
    }
    idx > other_idx
}

pub fn pick_assignable_host(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
) -> Option<ChosenHost> {
    pick_assignable_host_for(hosts, in_flight, IsolationClass::Process)
}

async fn workspace_need(store: &Store, project_id: Uuid) -> IsolationClass {
    match fleet_db::repos::workspaces::min_isolation(store.pool(), project_id).await {
        Ok(raw) => IsolationClass::parse(&raw).unwrap_or(IsolationClass::Process),
        Err(_) => IsolationClass::Process,
    }
}

/// Before `begin_spawn_assignment`: lowest load/cap, then oldest heartbeat,
/// among hosts that meet isolation and are under cap.
pub async fn select_assignable_host(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    let hosts = list_assignable_hosts(store, project_id).await?;
    if hosts.is_empty() {
        return None;
    }
    let in_flight = count_remote_in_flight(store).await?;
    let need = workspace_need(store, project_id).await;
    pick_assignable_host_in(&hosts, &in_flight, need, Some(project_id))
}

/// Today's host picker, then Native-only. No second list/query.
/// Cloud / Partners / public-cloud cannot be chosen.
/// Production tick does not fall back to an in-process runtime.
pub fn pick_assignable_runner_for(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
    need: IsolationClass,
) -> Option<ChosenHost> {
    let native: Vec<AssignableHost> = hosts
        .iter()
        .filter(|h| h.isolation.is_assignable())
        .cloned()
        .collect();
    pick_assignable_host_for(&native, in_flight, need)
}

pub fn pick_assignable_runner(
    hosts: &[AssignableHost],
    in_flight: &HashMap<Uuid, i64>,
) -> Option<ChosenHost> {
    pick_assignable_runner_for(hosts, in_flight, IsolationClass::Process)
}

/// Wraps [`select_assignable_host`]. Tick fallback is unchanged.
/// Cloud / Partners / public-cloud cannot be chosen.
pub async fn select_assignable_runner(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    select_assignable_host(store, project_id)
        .await
        .filter(|chosen| chosen.host.isolation.is_assignable())
}

/// Use `preferred` when that host is still assignable on this plane URL;
/// otherwise the usual picker. Loopback URLs drop host-runtime rows that
/// are not this machine *before* pick so a cluster/off-box winner cannot
/// starve the local box.
pub async fn select_assignable_runner_preferring(
    store: &Store,
    project_id: Uuid,
    preferred: Option<Uuid>,
    orchestrator_url: &str,
) -> Option<ChosenHost> {
    let hosts = list_assignable_hosts(store, project_id).await?;
    let need = workspace_need(store, project_id).await;
    let loopback = agent_fleet_contracts::url_is_loopback(orchestrator_url);
    let reachable: Vec<AssignableHost> = hosts
        .into_iter()
        .filter(|h| host_reachable_on_plane(h, need, loopback))
        .collect();
    if reachable.is_empty() {
        return None;
    }
    if let Some(id) = preferred {
        if let Some(host) = reachable.iter().find(|h| h.id == id) {
            if let Some(runtime) = effective_runtime_for(host, need) {
                return Some(ChosenHost {
                    host: host.clone(),
                    runtime,
                });
            }
        }
    }
    let in_flight = count_remote_in_flight(store).await?;
    pick_assignable_host_in(&reachable, &in_flight, need, Some(project_id))
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

/// A home host is mid-restart: do not fail over to cluster or local stub.
pub fn hold_ready_for_host_update(_has_assignable: bool, updating_recent: bool) -> bool {
    updating_recent
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

/// Ask the assigned `fleet-host` to stop a session (TERM/KILL/delete CR).
pub async fn enqueue_signal_session(
    store: &Store,
    workspace_id: Uuid,
    session_id: Uuid,
    host_id: Uuid,
    signal: &str,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "kind": "signal_session",
        "session_id": session_id,
        "host_id": host_id,
        "signal": signal,
    });
    let claim = fleet_db::NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id,
        kind: "signal_session".into(),
        payload,
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
            last_seen_at: h.last_seen_at,
            workspace_id: h.workspace_id,
            hostname: h.hostname.clone(),
            addrs: advertised_addrs(h),
        }
    }
}

fn advertised_addrs(h: &fleet_db::WorkerHost) -> Vec<String> {
    h.hardware
        .as_ref()
        .and_then(|hw| hw.get("addrs"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| {
            v.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn local_machine_names() -> Vec<String> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<String>, raw: &str| {
        let s = raw.trim();
        if s.is_empty() {
            return;
        }
        if !out.iter().any(|n| n.eq_ignore_ascii_case(s)) {
            out.push(s.to_string());
        }
    };
    if let Ok(s) = std::env::var("HOSTNAME") {
        push(&mut out, &s);
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        push(&mut out, &s);
    }
    if let Ok(outp) = std::process::Command::new("uname").arg("-n").output() {
        if outp.status.success() {
            push(&mut out, &String::from_utf8_lossy(&outp.stdout));
        }
    }
    if let Ok(outp) = std::process::Command::new("hostname").output() {
        if outp.status.success() {
            push(&mut out, &String::from_utf8_lossy(&outp.stdout));
        }
    }
    out
}

fn local_nonloopback_ips() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        for tok in String::from_utf8_lossy(&output.stdout).split_whitespace() {
            if tok != "127.0.0.1" && tok != "::1" {
                out.push(tok.to_string());
            }
        }
    }
    out
}

fn hostnames_match(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let al = a.split('.').next().unwrap_or(a);
    let bl = b.split('.').next().unwrap_or(b);
    !al.is_empty() && al.eq_ignore_ascii_case(bl)
}

/// Same rule as the plane card: hostname (short or FQDN) or advertised IP.
pub fn host_is_this_machine(host: &AssignableHost) -> bool {
    let names = local_machine_names();
    if let Some(hn) = host
        .hostname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if names.iter().any(|n| hostnames_match(hn, n)) {
            return true;
        }
    }
    let local_ips = local_nonloopback_ips();
    host.addrs
        .iter()
        .any(|a| local_ips.iter().any(|b| a.eq_ignore_ascii_case(b)))
}

fn host_reachable_on_plane(host: &AssignableHost, need: IsolationClass, loopback: bool) -> bool {
    if !host.isolation.is_assignable() {
        return false;
    }
    match effective_runtime_for(host, need) {
        Some(HostRuntime::Host) if loopback => host_is_this_machine(host),
        Some(HostRuntime::Host) | Some(HostRuntime::Container) => true,
        None => false,
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
                "no assignable host"
            );
            None
        }
    }
}

async fn count_remote_in_flight(store: &Store) -> Option<HashMap<Uuid, i64>> {
    let rows = sqlx::query_as::<_, (Option<String>, i64)>(
        r#"
        SELECT runtime_handle, COUNT(*)::bigint
        FROM worker_sessions
        WHERE runtime_backend = 'remote'
          AND status IN ('spawning', 'healthy')
        GROUP BY runtime_handle
        "#,
    )
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
            last_seen_at: None,
            workspace_id: Uuid::nil(),
            hostname: None,
            addrs: vec![],
        }
    }

    #[test]
    fn this_machine_matches_hostname_fqdn_and_addrs() {
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        assert!(
            !host_is_this_machine(&h),
            "missing hostname and addrs is not this machine"
        );
        h.hostname = Some("off-box-remote".into());
        assert!(!host_is_this_machine(&h));
        let local = local_machine_names();
        if let Some(name) = local.first() {
            h.hostname = Some(name.clone());
            assert!(host_is_this_machine(&h), "exact hostname {name}");
            h.hostname = Some(name.to_ascii_uppercase());
            assert!(host_is_this_machine(&h), "case-insensitive hostname");
            h.hostname = Some(format!("{name}.lan"));
            assert!(host_is_this_machine(&h), "FQDN vs short name");
        }
        h.hostname = None;
        let ips = local_nonloopback_ips();
        if let Some(ip) = ips.first() {
            h.addrs = vec![ip.clone()];
            assert!(host_is_this_machine(&h), "advertised IP {ip}");
        }
    }

    #[test]
    fn loopback_reachable_keeps_this_machine_drops_offbox_host() {
        let mut off = host(Uuid::new_v4(), true, false, true, false, 4);
        off.hostname = Some("off-box-remote".into());
        assert!(!host_reachable_on_plane(
            &off,
            IsolationClass::Process,
            true
        ));
        assert!(host_reachable_on_plane(
            &off,
            IsolationClass::Process,
            false
        ));
        let mut local = host(Uuid::new_v4(), true, false, true, false, 4);
        if let Some(name) = local_machine_names().into_iter().next() {
            local.hostname = Some(name);
            assert!(host_reachable_on_plane(
                &local,
                IsolationClass::Process,
                true
            ));
        }
        let container = host(Uuid::new_v4(), false, true, false, true, 4);
        assert!(
            host_reachable_on_plane(&container, IsolationClass::Process, true),
            "container guests stay in the pool; spawn refuses after assign"
        );
    }

    #[test]
    fn v1_assigns_host_even_when_container_is_advertised() {
        let h = host(Uuid::new_v4(), true, true, true, true, 4);
        assert_eq!(effective_runtime(&h), Some(HostRuntime::Host));
    }

    #[test]
    fn container_only_host_is_assignable() {
        let h = host(Uuid::new_v4(), true, true, false, true, 4);
        assert_eq!(effective_runtime(&h), Some(HostRuntime::Container));
        let chosen = pick_assignable_host(&[h.clone()], &HashMap::new()).unwrap();
        assert_eq!(chosen.runtime, HostRuntime::Container);
        assert_eq!(
            pick_assignable_host_for(&[h], &HashMap::new(), IsolationClass::Container)
                .unwrap()
                .runtime,
            HostRuntime::Container
        );
    }

    #[test]
    fn container_need_does_not_fall_back_to_process() {
        let process_only = host(Uuid::new_v4(), true, true, true, false, 4);
        assert!(pick_assignable_host_for(
            &[process_only],
            &HashMap::new(),
            IsolationClass::Container
        )
        .is_none());
    }

    #[test]
    fn container_need_picks_container_when_both_offered() {
        let h = host(Uuid::new_v4(), true, true, true, true, 4);
        let chosen =
            pick_assignable_host_for(&[h], &HashMap::new(), IsolationClass::Container).unwrap();
        assert_eq!(chosen.runtime, HostRuntime::Container);
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
    fn pick_lowest_utilization_not_first_under_capacity() {
        let busy = host(Uuid::new_v4(), true, false, true, false, 4);
        let idle = host(Uuid::new_v4(), true, false, true, false, 4);
        let mut inflight = HashMap::new();
        inflight.insert(busy.id, 3);
        let chosen = pick_assignable_host(&[busy.clone(), idle.clone()], &inflight).unwrap();
        assert_eq!(
            chosen.id(),
            idle.id,
            "3/4 must lose to 0/4 even when the busy host is listed first"
        );
    }

    #[test]
    fn pick_equal_load_prefers_oldest_not_newest_first() {
        let newer = host(Uuid::new_v4(), true, false, true, false, 4);
        let older = host(Uuid::new_v4(), true, false, true, false, 4);
        let chosen =
            pick_assignable_host(&[newer.clone(), older.clone()], &HashMap::new()).unwrap();
        assert_eq!(
            chosen.id(),
            older.id,
            "tie-break is oldest heartbeat, not SQL last_seen DESC list order"
        );
    }

    #[test]
    fn vm_need_is_unassigned_when_no_host_offers_vm() {
        let container = host(Uuid::new_v4(), true, true, true, true, 4);
        assert!(
            pick_assignable_host_for(&[container], &HashMap::new(), IsolationClass::Vm).is_none(),
            "min_isolation=vm must not fall back to container"
        );
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
    fn hold_ready_when_home_host_is_updating() {
        assert!(hold_ready_for_host_update(false, true));
        assert!(
            hold_ready_for_host_update(true, true),
            "do not fail over to cluster while a home host restarts"
        );
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
            agent_runtimes: serde_json::json!([]),
            settings_gen: 0,
            settings_ack_gen: 0,
            install_request: None,
            install_requested_at: None,
            runtime_settings: serde_json::json!({}),
            runtime_reported: serde_json::json!({}),
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
