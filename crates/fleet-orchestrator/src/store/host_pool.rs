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

/// Isolation of a runner. Copied from `worker_hosts.isolation`.
/// Sandboxed is assignable only when the host offers container or VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerIsolation {
    #[default]
    Native,
    Sandboxed,
}

impl RunnerIsolation {
    /// Native is always a picker candidate. Sandboxed is assigned only via
    /// `host_reachable_on_plane` when the host offers container/VM.
    pub fn is_assignable(self) -> bool {
        matches!(self, Self::Native)
    }
}

/// Product execution kind. A paired host of any kind is assignable when
/// it offers an isolation class that meets workspace `min_isolation`.
/// Cloud / Partners / Fleet Cloud must be sandboxed (never native exec).
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
        true
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
    pub created_by: Option<Uuid>,
    pub send_policy: String,
    pub send_allowlist: Vec<Uuid>,
    pub labels: Vec<String>,
    pub run_mode: String,
    pub settings_acked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRuntime {
    Host,
    Container,
    Vm,
}

impl HostRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Container => "container",
            Self::Vm => "vm",
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

/// Cluster label → not a PC (chat must not use it).
pub fn host_is_not_a_pc(host: &AssignableHost) -> bool {
    host.labels
        .iter()
        .any(|l| fleet_db::label_is_cluster(l))
}

/// Who may send work to this PC. Cluster → false.
/// No pairer and not cluster: unowned box (tests / bootstrap without label) is open.
pub fn send_acl_allows(host: &AssignableHost, actor: Option<Uuid>, is_member: bool) -> bool {
    if host_is_not_a_pc(host) {
        return false;
    }
    let Some(owner) = host.created_by else {
        return true;
    };
    let Some(actor) = actor else {
        return false;
    };
    match host.send_policy.as_str() {
        fleet_db::WORKER_HOST_SEND_WORKSPACE => is_member,
        fleet_db::WORKER_HOST_SEND_ALLOWLIST => {
            if host.send_allowlist.is_empty() {
                owner == actor
            } else {
                host.send_allowlist.contains(&actor)
            }
        }
        _ => owner == actor,
    }
}

/// Tick in this slice: send-ACL on real PCs only. Kind/bootstrap still assigns.
pub fn pr1_tick_allows(host: &AssignableHost, actor: Option<Uuid>, is_member: bool) -> bool {
    if host_is_not_a_pc(host) {
        return true;
    }
    send_acl_allows(host, actor, is_member)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    OnPc,
    InContainer,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnPc => fleet_db::WORKER_HOST_RUN_ON_PC,
            Self::InContainer => fleet_db::WORKER_HOST_RUN_IN_CONTAINER,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match fleet_db::parse_worker_host_run_mode(raw)? {
            fleet_db::WORKER_HOST_RUN_IN_CONTAINER => Some(Self::InContainer),
            _ => Some(Self::OnPc),
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobNeed {
    pub run_mode: RunMode,
    pub actor: Option<Uuid>,
    pub min_isolation: IsolationClass,
    pub provider: Option<String>,
    /// True when a matching workspace vendor key exists (or the job is mock/lab).
    pub vendor_key_ok: bool,
}

fn in_container_vendor_ok(job: &JobNeed) -> bool {
    match job
        .provider
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "mock" | "off" | "none" | "bash" | "ollama" => true,
        "grok" | "claude" | "anthropic" | "xai" => job.vendor_key_ok,
        _ => job.vendor_key_ok,
    }
}

pub fn runtime_for_run_mode(mode: RunMode) -> HostRuntime {
    match mode {
        RunMode::OnPc => HostRuntime::Host,
        RunMode::InContainer => HostRuntime::Container,
    }
}

/// Only assign predicate after chunk 2. No process→container upgrade.
pub fn host_matches_job(host: &AssignableHost, job: &JobNeed, is_member: bool) -> bool {
    if !host.isolation.is_assignable() {
        return false;
    }
    if !host.settings_acked {
        return false;
    }
    let host_mode = RunMode::parse(&host.run_mode).unwrap_or(RunMode::OnPc);
    if host_mode != job.run_mode {
        return false;
    }
    match job.run_mode {
        RunMode::OnPc => {
            host.allow_host
                && host.advertised_host
                && send_acl_allows(host, job.actor, is_member)
                && job.min_isolation <= IsolationClass::Process
        }
        RunMode::InContainer => {
            host.allow_container
                && host.advertised_container
                && job.min_isolation <= IsolationClass::Container
                && in_container_vendor_ok(job)
        }
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
        // A container-capable sandboxed host can also meet a VM *need* only
        // when the host later advertises hypervisor. Process-only stays process.
    }
    offer
}

pub fn runtime_for_class(class: IsolationClass) -> Option<HostRuntime> {
    match class {
        IsolationClass::Process => Some(HostRuntime::Host),
        IsolationClass::Container => Some(HostRuntime::Container),
        IsolationClass::Vm => Some(HostRuntime::Vm),
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

/// Tick assignment honors workspace `min_isolation`. Process need → OnPc;
/// container need → InContainer. Cloud / Partners cannot be chosen.
pub async fn select_assignable_runner(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    select_assignable_runner_for_actor(store, project_id, None).await
}

fn run_mode_for_isolation(need: IsolationClass) -> Option<RunMode> {
    match need {
        IsolationClass::Process => Some(RunMode::OnPc),
        IsolationClass::Container => Some(RunMode::InContainer),
        IsolationClass::Vm => None,
    }
}

/// Chat / inbox taking-runner: send-ACL on (cluster is not a PC).
pub async fn select_assignable_runner_for_actor(
    store: &Store,
    project_id: Uuid,
    actor: Option<Uuid>,
) -> Option<ChosenHost> {
    let hosts = list_assignable_hosts(store, project_id).await?;
    let need = workspace_need(store, project_id).await;
    let run_mode = run_mode_for_isolation(need)?;
    let member = actor_is_member(store, project_id, actor).await;
    let job = JobNeed {
        run_mode,
        actor,
        min_isolation: need,
        provider: None,
        vendor_key_ok: true,
    };
    let matching: Vec<AssignableHost> = hosts
        .into_iter()
        .filter(|h| host_matches_job(h, &job, member))
        .collect();
    if matching.is_empty() {
        return None;
    }
    let in_flight = count_remote_in_flight(store).await?;
    pick_assignable_host_in(&matching, &in_flight, need, Some(project_id))
}

/// Online native host-runtime, even at job cap. Inbox finish stamps this so
/// the ticket waits instead of 400 when every box is busy.
pub async fn select_online_runner(store: &Store, project_id: Uuid) -> Option<ChosenHost> {
    select_online_runner_for_actor(store, project_id, None).await
}

pub async fn select_online_runner_for_actor(
    store: &Store,
    project_id: Uuid,
    actor: Option<Uuid>,
) -> Option<ChosenHost> {
    let scoped = match fleet_db::repos::worker_hosts::list_online_runners(
        store.pool(),
        project_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    {
        Ok(h) => h,
        Err(_) => return None,
    };
    let extra = fleet_db::repos::worker_hosts::list_online_by_hostnames(
        store.pool(),
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
        &local_machine_names(),
    )
    .await
    .unwrap_or_default();
    let mut hosts: Vec<AssignableHost> = scoped.iter().map(AssignableHost::from).collect();
    for h in extra {
        if hosts.iter().any(|x| x.id == h.id) {
            continue;
        }
        let ah = AssignableHost::from(&h);
        if host_advertises_local_addr(&ah) {
            hosts.push(ah);
        }
    }
    attach_send_allowlists(store, &mut hosts).await;
    let member = actor_is_member(store, project_id, actor).await;
    let need = workspace_need(store, project_id).await;
    let run_mode = run_mode_for_isolation(need)?;
    let job = JobNeed {
        run_mode,
        actor,
        min_isolation: need,
        provider: None,
        vendor_key_ok: true,
    };
    hosts.into_iter().find_map(|host| {
        if !host_matches_job(&host, &job, member) {
            return None;
        }
        let runtime = runtime_for_run_mode(run_mode);
        Some(ChosenHost { host, runtime })
    })
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
    select_assignable_runner_preferring_for_actor(
        store,
        project_id,
        preferred,
        orchestrator_url,
        None,
    )
    .await
}

pub async fn select_assignable_runner_preferring_for_actor(
    store: &Store,
    project_id: Uuid,
    preferred: Option<Uuid>,
    orchestrator_url: &str,
    actor: Option<Uuid>,
) -> Option<ChosenHost> {
    let need = workspace_need(store, project_id).await;
    let run_mode = run_mode_for_isolation(need)?;
    let job = JobNeed {
        run_mode,
        actor,
        min_isolation: need,
        provider: None,
        vendor_key_ok: true,
    };
    select_assignable_runner_preferring_for_job(
        store,
        project_id,
        preferred,
        orchestrator_url,
        &job,
    )
    .await
}

pub async fn select_assignable_runner_preferring_for_job(
    store: &Store,
    project_id: Uuid,
    preferred: Option<Uuid>,
    orchestrator_url: &str,
    job: &JobNeed,
) -> Option<ChosenHost> {
    let hosts = list_assignable_hosts(store, project_id).await?;
    let member = actor_is_member(store, project_id, job.actor).await;
    let loopback = agent_fleet_contracts::url_is_loopback(orchestrator_url);
    let reachable: Vec<AssignableHost> = hosts
        .into_iter()
        .filter(|h| host_reachable_on_plane(h, job.min_isolation, loopback))
        .filter(|h| host_matches_job(h, job, member))
        .collect();
    if reachable.is_empty() {
        return None;
    }
    let runtime = runtime_for_run_mode(job.run_mode);
    if let Some(id) = preferred {
        if let Some(host) = reachable.iter().find(|h| h.id == id) {
            return Some(ChosenHost {
                host: host.clone(),
                runtime,
            });
        }
    }
    let in_flight = count_remote_in_flight(store).await?;
    pick_assignable_host_in(&reachable, &in_flight, job.min_isolation, Some(project_id)).map(
        |mut chosen| {
            chosen.runtime = runtime;
            chosen
        },
    )
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
            created_by: h.created_by,
            send_policy: h.send_policy.clone(),
            send_allowlist: Vec::new(),
            labels: h.labels.clone(),
            run_mode: h.run_mode.clone(),
            settings_acked: h.settings_gen == h.settings_ack_gen,
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
    host_advertises_local_addr(host)
}

/// Cross-workspace share: advertised non-loopback IP ∩ this plane.
/// Hostname alone is impersonable (`ubuntu` on two boxes).
fn host_advertises_local_addr(host: &AssignableHost) -> bool {
    let local_ips = local_nonloopback_ips();
    host.addrs
        .iter()
        .any(|a| local_ips.iter().any(|b| a.eq_ignore_ascii_case(b)))
}

fn host_reachable_on_plane(host: &AssignableHost, need: IsolationClass, loopback: bool) -> bool {
    if matches!(host.isolation, RunnerIsolation::Sandboxed)
        && !offered_classes(host)
            .iter()
            .any(|c| matches!(c, IsolationClass::Container | IsolationClass::Vm))
    {
        return false;
    }
    match effective_runtime_for(host, need) {
        Some(HostRuntime::Host) if matches!(host.isolation, RunnerIsolation::Sandboxed) => false,
        Some(HostRuntime::Host) if loopback => host_is_this_machine(host),
        Some(HostRuntime::Host) | Some(HostRuntime::Container) | Some(HostRuntime::Vm) => true,
        None => false,
    }
}

async fn list_assignable_hosts(store: &Store, project_id: Uuid) -> Option<Vec<AssignableHost>> {
    let scoped = match fleet_db::repos::worker_hosts::list_assignable(
        store.pool(),
        project_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(
                error = %e,
                project_id = %project_id,
                "no assignable host"
            );
            return None;
        }
    };
    let extra = fleet_db::repos::worker_hosts::list_assignable_by_hostnames(
        store.pool(),
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
        &local_machine_names(),
    )
    .await
    .unwrap_or_default();
    let mut hosts: Vec<AssignableHost> = scoped.iter().map(AssignableHost::from).collect();
    for h in extra {
        if hosts.iter().any(|x| x.id == h.id) {
            continue;
        }
        let ah = AssignableHost::from(&h);
        if host_advertises_local_addr(&ah) {
            hosts.push(ah);
        }
    }
    attach_send_allowlists(store, &mut hosts).await;
    Some(hosts)
}

async fn attach_send_allowlists(store: &Store, hosts: &mut [AssignableHost]) {
    let ids: Vec<Uuid> = hosts.iter().map(|h| h.id).collect();
    let map = match fleet_db::repos::worker_hosts::list_send_allowlists_for_hosts(
        store.pool(),
        &ids,
    )
    .await
    {
        Ok(m) => m,
        Err(_) => return,
    };
    for h in hosts.iter_mut() {
        if let Some(ids) = map.get(&h.id) {
            h.send_allowlist = ids.clone();
        }
    }
}

async fn actor_is_member(store: &Store, workspace_id: Uuid, actor: Option<Uuid>) -> bool {
    let Some(uid) = actor else {
        return false;
    };
    fleet_db::repos::workspaces::is_member(store.pool(), workspace_id, uid)
        .await
        .unwrap_or(false)
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
            created_by: None,
            send_policy: fleet_db::WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            send_allowlist: vec![],
            labels: vec![],
            run_mode: fleet_db::WORKER_HOST_RUN_ON_PC.to_string(),
            settings_acked: true,
        }
    }

    fn require_local_name() -> String {
        local_machine_names().into_iter().next().expect(
            "hostname match needs a local name from HOSTNAME, /etc/hostname, uname -n, or hostname",
        )
    }

    fn require_local_ip() -> String {
        local_nonloopback_ips().into_iter().next().expect(
            "advertised-IP match needs a non-loopback address from hostname -I",
        )
    }

    #[test]
    fn host_is_this_machine_empty_hostname_and_addrs_is_not_this_machine() {
        let h = host(Uuid::new_v4(), true, false, true, false, 4);
        assert!(
            !host_is_this_machine(&h),
            "missing hostname and addrs is not this machine"
        );
    }

    #[test]
    fn host_share_requires_advertised_local_ip_not_hostname_alone() {
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some(require_local_name());
        h.addrs.clear();
        assert!(
            !host_advertises_local_addr(&h),
            "same hostname without a local advertised IP must not share across workspaces"
        );
        let ip = require_local_ip();
        h.addrs = vec![ip.clone()];
        assert!(
            host_advertises_local_addr(&h),
            "share requires local IP {ip}"
        );
    }

    #[test]
    fn host_is_this_machine_offbox_hostname_is_not_this_machine() {
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some("off-box-remote".into());
        assert!(
            !host_is_this_machine(&h),
            "off-box-remote must not match this machine"
        );
    }

    #[test]
    fn host_is_this_machine_matches_exact_local_hostname() {
        let name = require_local_name();
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some(name.clone());
        assert!(host_is_this_machine(&h), "exact hostname {name}");
    }

    #[test]
    fn host_is_this_machine_matches_local_hostname_case_insensitively() {
        let name = require_local_name();
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some(name.to_ascii_uppercase());
        assert!(
            host_is_this_machine(&h),
            "case-insensitive hostname {name}"
        );
    }

    #[test]
    fn host_is_this_machine_matches_advertised_fqdn_against_local_name() {
        let name = require_local_name();
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some(format!("{name}.lan"));
        assert!(host_is_this_machine(&h), "FQDN vs short name {name}.lan");
    }

    #[test]
    fn host_is_this_machine_matches_advertised_nonloopback_ip() {
        let ip = require_local_ip();
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.addrs = vec![ip.clone()];
        assert!(host_is_this_machine(&h), "advertised IP {ip}");
    }

    #[test]
    fn host_is_this_machine_matches_short_label_when_local_name_is_fqdn() {
        assert!(
            hostnames_match("box", "box.lan"),
            "short label must match FQDN"
        );
        assert!(
            hostnames_match("box.lan", "box"),
            "FQDN must match short label"
        );
        assert!(
            hostnames_match("BOX", "box.lan"),
            "short/FQDN match is case-insensitive"
        );
    }

    #[test]
    fn host_is_this_machine_loopback_addrs_are_not_this_machine() {
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = None;
        h.addrs = vec!["127.0.0.1".into(), "::1".into()];
        assert!(
            !host_is_this_machine(&h),
            "loopback addrs are filtered by local_nonloopback_ips"
        );
    }

    #[test]
    fn host_is_this_machine_whitespace_only_hostname_is_not_this_machine() {
        let mut h = host(Uuid::new_v4(), true, false, true, false, 4);
        h.hostname = Some("  \n".into());
        assert!(
            !host_is_this_machine(&h),
            "whitespace-only hostname is trimmed away and is not this machine"
        );
    }

    #[test]
    fn loopback_drops_offbox_host() {
        let mut off = host(Uuid::new_v4(), true, false, true, false, 4);
        off.hostname = Some("off-box-remote".into());
        assert!(
            !host_reachable_on_plane(&off, IsolationClass::Process, true),
            "off-box-remote host-runtime cannot heartbeat to a loopback plane"
        );
    }

    #[test]
    fn non_loopback_keeps_offbox_host() {
        let mut off = host(Uuid::new_v4(), true, false, true, false, 4);
        off.hostname = Some("off-box-remote".into());
        assert!(
            host_reachable_on_plane(&off, IsolationClass::Process, false),
            "off-box-remote stays reachable when the plane is not loopback"
        );
    }

    #[test]
    fn loopback_keeps_this_machine_by_hostname() {
        let name = require_local_name();
        let mut local = host(Uuid::new_v4(), true, false, true, false, 4);
        local.hostname = Some(name.clone());
        assert!(
            host_reachable_on_plane(&local, IsolationClass::Process, true),
            "this-machine {name} stays reachable on a loopback plane"
        );
    }

    #[test]
    fn loopback_drops_host_runtime_with_empty_identity() {
        let nameless = host(Uuid::new_v4(), true, false, true, false, 4);
        assert!(
            !host_reachable_on_plane(&nameless, IsolationClass::Process, true),
            "Host+loopback with hostname None and empty addrs is not this machine"
        );
    }

    #[test]
    fn loopback_drops_sandboxed_even_when_hostname_is_this_machine() {
        let name = require_local_name();
        let mut sandboxed = host(Uuid::new_v4(), true, false, true, false, 4);
        sandboxed.hostname = Some(name.clone());
        sandboxed.isolation = RunnerIsolation::Sandboxed;
        assert!(
            !host_reachable_on_plane(&sandboxed, IsolationClass::Process, true),
            "sandboxed isolation is not assignable even when hostname is this machine ({name})"
        );
    }

    #[test]
    fn loopback_keeps_this_machine_by_advertised_ip() {
        let ip = require_local_ip();
        let mut local = host(Uuid::new_v4(), true, false, true, false, 4);
        local.hostname = None;
        local.addrs = vec![ip.clone()];
        assert!(
            host_reachable_on_plane(&local, IsolationClass::Process, true),
            "this-machine advertised IP {ip} stays reachable on a loopback plane"
        );
    }

    #[test]
    fn container_only_host_is_assignable() {
        let container_only = host(Uuid::new_v4(), true, true, false, true, 4);
        let chosen = pick_assignable_host(&[container_only], &HashMap::new())
            .expect("container-only host must still be assignable");
        assert_eq!(
            chosen.runtime,
            HostRuntime::Container,
            "process need upgrades to Container when advertised_host is off"
        );
    }

    #[test]
    fn plane_disallows_host_still_assigns_container() {
        let h = host(Uuid::new_v4(), false, true, true, true, 4);
        assert_eq!(
            effective_runtime(&h),
            Some(HostRuntime::Container),
            "allow_host=false still offers Container when advertised_container is on"
        );
    }

    #[test]
    fn pick_assignable_runner_assigns_container_only() {
        let container_only = host(Uuid::new_v4(), true, true, false, true, 4);
        let chosen = pick_assignable_runner(&[container_only], &HashMap::new())
            .expect("container-only native host must still be assignable");
        assert_eq!(
            chosen.runtime,
            HostRuntime::Container,
            "pick_assignable_runner keeps Container; V1 select_assignable_runner is host-only"
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
    fn pick_assigns_when_inflight_is_one_below_plane_max() {
        let id = Uuid::new_v4();
        let h = host(id, true, true, true, true, 4);
        let mut inflight = HashMap::new();
        inflight.insert(id, 3);
        let chosen = pick_assignable_host(&[h], &inflight)
            .expect("inflight 3 is under plane max 4");
        assert_eq!(chosen.id(), id);
    }

    #[test]
    fn pick_none_when_inflight_hits_clamped_256() {
        let id = Uuid::new_v4();
        let over = host(id, true, true, true, true, 999);
        let mut at_ceiling = HashMap::new();
        at_ceiling.insert(id, 256);
        assert!(
            pick_assignable_host(&[over.clone()], &at_ceiling).is_none(),
            "plane max 999 clamps to 256; inflight 256 must skip"
        );
        let mut just_under = HashMap::new();
        just_under.insert(id, 255);
        let chosen = pick_assignable_host(&[over], &just_under)
            .expect("inflight 255 is still under the 256 ceiling");
        assert_eq!(chosen.id(), id);
    }

    #[test]
    fn host_at_capacity_clamps_nonpositive_plane_max_to_1() {
        let zero = host(Uuid::new_v4(), true, true, true, true, 0);
        assert!(
            host_at_capacity(1, &zero),
            "plane max 0 clamps to 1"
        );
        assert!(
            !host_at_capacity(0, &zero),
            "clamp 1 still allows the first job"
        );
        let neg = host(Uuid::new_v4(), true, true, true, true, -5);
        assert!(
            host_at_capacity(1, &neg),
            "plane max -5 clamps to 1"
        );
        assert!(
            !host_at_capacity(0, &neg),
            "clamp 1 still allows the first job"
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
            runner_kind: fleet_db::RUNNER_KIND_SELF_HOSTED.into(),
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
            send_policy: fleet_db::WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            run_mode: fleet_db::WORKER_HOST_RUN_ON_PC.to_string(),
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

    /// 0/0 load: prefer_workspace beats an older cluster row listed first.
    #[test]
    fn pick_load_tie_prefers_home_workspace_over_cluster() {
        let home_ws = Uuid::new_v4();
        let cluster_ws = Uuid::new_v4();
        let older = chrono::Utc::now() - chrono::Duration::seconds(60);
        let newer = chrono::Utc::now();

        let mut cluster = host(Uuid::new_v4(), true, false, true, false, 4);
        cluster.workspace_id = cluster_ws;
        cluster.last_seen_at = Some(older);

        let mut home_host = host(Uuid::new_v4(), true, false, true, false, 4);
        home_host.workspace_id = home_ws;
        home_host.last_seen_at = Some(newer);

        let chosen = pick_assignable_host_in(
            &[cluster.clone(), home_host.clone()],
            &HashMap::new(),
            IsolationClass::Process,
            Some(home_ws),
        )
        .expect("home host must be assignable");
        assert_eq!(
            chosen.id(),
            home_host.id,
            "0/0 load: home workspace beats an older cluster host listed first"
        );

        let chosen_rev = pick_assignable_host_in(
            &[home_host.clone(), cluster],
            &HashMap::new(),
            IsolationClass::Process,
            Some(home_ws),
        )
        .expect("home host must be assignable");
        assert_eq!(
            chosen_rev.id(),
            home_host.id,
            "list order must not change the home winner"
        );
    }

    #[test]
    fn send_acl_owner_only_is_pairer() {
        let maya = Uuid::new_v4();
        let jordan = Uuid::new_v4();
        let mut pc = host(Uuid::new_v4(), true, true, true, true, 4);
        pc.created_by = Some(maya);
        pc.send_policy = fleet_db::WORKER_HOST_SEND_OWNER_ONLY.to_string();
        assert!(send_acl_allows(&pc, Some(maya), true));
        assert!(!send_acl_allows(&pc, Some(jordan), true));
        assert!(!send_acl_allows(&pc, None, true));
    }

    #[test]
    fn send_acl_cluster_is_not_a_pc() {
        let maya = Uuid::new_v4();
        let mut cluster = host(Uuid::new_v4(), true, true, true, true, 4);
        cluster.created_by = None;
        cluster.labels = vec!["cluster".into()];
        assert!(host_is_not_a_pc(&cluster));
        assert!(!send_acl_allows(&cluster, Some(maya), true));
        assert!(pr1_tick_allows(&cluster, Some(maya), true));
        assert!(pr1_tick_allows(&cluster, None, false));
    }

    #[test]
    fn send_acl_workspace_rechecks_membership() {
        let maya = Uuid::new_v4();
        let jordan = Uuid::new_v4();
        let mut pc = host(Uuid::new_v4(), true, true, true, true, 4);
        pc.created_by = Some(maya);
        pc.send_policy = fleet_db::WORKER_HOST_SEND_WORKSPACE.to_string();
        assert!(send_acl_allows(&pc, Some(jordan), true));
        assert!(!send_acl_allows(&pc, Some(jordan), false));
    }

    #[test]
    fn send_acl_empty_allowlist_is_owner_only() {
        let maya = Uuid::new_v4();
        let jordan = Uuid::new_v4();
        let mut pc = host(Uuid::new_v4(), true, true, true, true, 4);
        pc.created_by = Some(maya);
        pc.send_policy = fleet_db::WORKER_HOST_SEND_ALLOWLIST.to_string();
        pc.send_allowlist = vec![];
        assert!(send_acl_allows(&pc, Some(maya), true));
        assert!(!send_acl_allows(&pc, Some(jordan), true));
        pc.send_allowlist = vec![jordan];
        assert!(send_acl_allows(&pc, Some(jordan), true));
        assert!(!send_acl_allows(&pc, Some(maya), true));
    }

    #[test]
    fn host_matches_job_does_not_cross_modes() {
        let maya = Uuid::new_v4();
        let mut pc = host(Uuid::new_v4(), true, true, true, true, 4);
        pc.created_by = Some(maya);
        pc.run_mode = fleet_db::WORKER_HOST_RUN_ON_PC.to_string();
        let on_pc = JobNeed {
            run_mode: RunMode::OnPc,
            actor: Some(maya),
            min_isolation: IsolationClass::Process,
            provider: None,
            vendor_key_ok: true,
        };
        let in_c = JobNeed {
            run_mode: RunMode::InContainer,
            actor: Some(maya),
            min_isolation: IsolationClass::Process,
            provider: None,
            vendor_key_ok: true,
        };
        assert!(host_matches_job(&pc, &on_pc, true));
        assert!(!host_matches_job(&pc, &in_c, true));
        pc.run_mode = fleet_db::WORKER_HOST_RUN_IN_CONTAINER.to_string();
        assert!(!host_matches_job(&pc, &on_pc, true));
        assert!(host_matches_job(&pc, &in_c, true));
    }
}
