use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Served dist identity vs the box report (`HostPublic.software_status`).
pub const WORKER_SOFTWARE_STATUS_UNKNOWN: &str = "unknown";
pub const WORKER_SOFTWARE_STATUS_CURRENT: &str = "current";
pub const WORKER_SOFTWARE_STATUS_BEHIND: &str = "behind";

pub const WORKER_HOST_STATUS_PENDING: &str = "pending";
pub const WORKER_HOST_STATUS_ONLINE: &str = "online";
pub const WORKER_HOST_STATUS_DISABLED: &str = "disabled";
pub const WORKER_HOST_STATUS_ERROR: &str = "error";

/// Native host process (v1). Only this isolation is assignable.
pub const WORKER_HOST_KIND_HOST: &str = "host";
pub const WORKER_HOST_KIND_CONTROL_PLANE: &str = "control_plane";

/// Bootstrap-only guest label. HTTP PATCH must not stamp this.
pub const WORKER_HOST_LABEL_CLUSTER: &str = "cluster";

pub const WORKER_HOST_ISOLATION_NATIVE: &str = "native";
/// Placeholder self-hosted sandbox. Not assignable; no k8s executor in v1.
pub const WORKER_HOST_ISOLATION_SANDBOXED: &str = "sandboxed";

/// Only the pairer (`created_by`) may send work to this PC.
pub const WORKER_HOST_SEND_OWNER_ONLY: &str = "owner_only";
/// Explicit user ids. Empty allowlist is treated as owner_only at assign time.
pub const WORKER_HOST_SEND_ALLOWLIST: &str = "allowlist";
/// Any current workspace member may send work to this shared PC.
pub const WORKER_HOST_SEND_WORKSPACE: &str = "workspace";

pub const WORKER_HOST_RUN_ON_PC: &str = "on_pc";
pub const WORKER_HOST_RUN_IN_CONTAINER: &str = "in_container";

/// `on_pc` | `in_container`. Other values are rejected.
pub fn parse_worker_host_run_mode(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        WORKER_HOST_RUN_ON_PC => Some(WORKER_HOST_RUN_ON_PC),
        WORKER_HOST_RUN_IN_CONTAINER => Some(WORKER_HOST_RUN_IN_CONTAINER),
        _ => None,
    }
}

/// `owner_only` | `allowlist` | `workspace`. Other values are rejected.
pub fn parse_worker_host_send_policy(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        WORKER_HOST_SEND_OWNER_ONLY => Some(WORKER_HOST_SEND_OWNER_ONLY),
        WORKER_HOST_SEND_ALLOWLIST => Some(WORKER_HOST_SEND_ALLOWLIST),
        WORKER_HOST_SEND_WORKSPACE => Some(WORKER_HOST_SEND_WORKSPACE),
        _ => None,
    }
}

/// `native` or `sandboxed`. Other values are rejected.
pub fn parse_worker_host_isolation(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        WORKER_HOST_ISOLATION_NATIVE => Some(WORKER_HOST_ISOLATION_NATIVE),
        WORKER_HOST_ISOLATION_SANDBOXED => Some(WORKER_HOST_ISOLATION_SANDBOXED),
        _ => None,
    }
}

/// Trimmed, case-insensitive match for [`WORKER_HOST_LABEL_CLUSTER`].
/// Same rule as list_cluster / list_assignable SQL.
pub fn label_is_cluster(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case(WORKER_HOST_LABEL_CLUSTER)
}

/// Trim labels; store the cluster token as lowercase `cluster`.
pub fn normalize_host_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter_map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if label_is_cluster(trimmed) {
                Some(WORKER_HOST_LABEL_CLUSTER.to_string())
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

/// Stored `online` older than this is serialized as `offline`.
///
/// Heartbeats are every ~10s. 90s is several missed beats plus a Wi-Fi roam,
/// so a laptop does not flicker offline between successful heartbeats.
pub const WORKER_HOST_OFFLINE_AFTER: Duration = Duration::seconds(90);

/// Durable paired worker machine (`worker_hosts`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct WorkerHost {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub paused: bool,
    pub drain: bool,
    /// `native` (v1) or `sandboxed` (placeholder; not assignable).
    pub isolation: String,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
    #[serde(skip_serializing)]
    pub machine_token_hash: Option<String>,
    pub advertised_host: bool,
    pub advertised_container: bool,
    pub platform: Option<String>,
    pub hostname: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub current_jobs: i32,
    pub advertised_max_concurrent: Option<i32>,
    pub software_git_sha: Option<String>,
    pub software_bins: Option<serde_json::Value>,
    pub software_reported_at: Option<DateTime<Utc>>,
    pub software_updating: bool,
    pub auto_update: bool,
    pub update_requested_at: Option<DateTime<Utc>>,
    pub machine_id: Option<Uuid>,
    pub hardware: Option<serde_json::Value>,
    #[serde(default = "default_agent_runtimes")]
    pub agent_runtimes: serde_json::Value,
    pub settings_gen: i32,
    pub settings_ack_gen: i32,
    #[serde(default)]
    pub install_request: Option<serde_json::Value>,
    #[serde(default)]
    pub install_requested_at: Option<DateTime<Utc>>,
    /// Desired box-local knobs (work dir, Ollama models dir, k8s). Applied on settings_gen.
    #[serde(default = "default_json_object")]
    pub runtime_settings: serde_json::Value,
    /// Last heartbeat snapshot of LocalConfig (informational).
    #[serde(default = "default_json_object")]
    pub runtime_reported: serde_json::Value,
    pub created_by: Option<Uuid>,
    /// Who may send work to this PC (`owner_only` | `allowlist` | `workspace`).
    #[serde(default = "default_send_policy")]
    pub send_policy: String,
    /// `on_pc` | `in_container`.
    #[serde(default = "default_run_mode")]
    pub run_mode: String,
    pub created_at: DateTime<Utc>,
    pub paired_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

fn default_json_object() -> serde_json::Value {
    serde_json::json!({})
}

impl WorkerHost {
    /// Plane ∩ advertised. Worker-local `run_*` is applied on the box, not here.
    pub fn effective_host(&self) -> bool {
        self.allow_host && self.advertised_host
    }

    pub fn effective_container(&self) -> bool {
        self.allow_container && self.advertised_container
    }

    /// `/me/lease`: native + plane ∩ advertised host.
    /// Container-only *cluster* hosts (Kind `fleet-host`) also lease so they
    /// can create FleetSandbox CRs. A paired k3d-only box must not take
    /// host-runtime spawn_session.
    pub fn accepts_v1_leases(&self) -> bool {
        if !self.enabled
            || self.paused
            || self.drain
            || !self.is_native_isolation()
            || self.status != WORKER_HOST_STATUS_ONLINE
        {
            return false;
        }
        match self.run_mode.as_str() {
            WORKER_HOST_RUN_IN_CONTAINER => self.effective_container(),
            _ => self.effective_host(),
        }
    }

    /// Tick / lease assignment: only native isolation is v1.
    pub fn is_native_isolation(&self) -> bool {
        self.isolation == WORKER_HOST_ISOLATION_NATIVE
    }

    /// Bootstrap-only guest on every workspace. HTTP PATCH must not stamp this.
    pub fn is_cluster_host(&self) -> bool {
        self.labels.iter().any(|l| label_is_cluster(l))
    }

    pub fn settings_pending(&self) -> bool {
        self.settings_gen != self.settings_ack_gen
    }

    /// `online` with a stale `last_seen_at` is reported as `offline`.
    pub fn public_status(&self, now: DateTime<Utc>) -> &str {
        if self.status == WORKER_HOST_STATUS_ONLINE {
            match self.last_seen_at {
                Some(seen) if now.signed_duration_since(seen) <= WORKER_HOST_OFFLINE_AFTER => {
                    WORKER_HOST_STATUS_ONLINE
                }
                _ => "offline",
            }
        } else {
            &self.status
        }
    }

    pub fn as_public(&self, now: DateTime<Utc>) -> WorkerHostPublic {
        WorkerHostPublic::from_host(self, now)
    }
}

/// API/UI shape — never includes hashes or raw tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHostPublic {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    /// `host` (paired fleet-host) or `control_plane` (this API process).
    #[serde(default = "default_host_kind")]
    pub kind: String,
    pub status: String,
    pub enabled: bool,
    pub paused: bool,
    pub drain: bool,
    #[serde(default = "default_isolation")]
    pub isolation: String,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
    pub advertised_host: bool,
    pub advertised_container: bool,
    pub platform: Option<String>,
    pub hostname: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub paired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub software: Option<WorkerSoftwarePublic>,
    /// Staged `manifest.json` for `platform`, if that platform was built.
    #[serde(default)]
    pub desired: Option<WorkerSoftwareManifest>,
    /// `current` | `behind` | `unknown`. Never `behind` without a report + manifest.
    #[serde(default = "default_software_status")]
    pub software_status: String,
    /// True only when `software_status` is `behind`.
    #[serde(default)]
    pub outdated: bool,
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
    #[serde(default)]
    pub update_requested_at: Option<DateTime<Utc>>,
    /// Durable box id (`fleet-host` `machine.id`). Same machine after re-pair.
    #[serde(default)]
    pub machine_id: Option<Uuid>,
    #[serde(default)]
    pub settings_gen: i32,
    #[serde(default)]
    pub settings_ack_gen: i32,
    #[serde(default)]
    pub settings_pending: bool,
    #[serde(default)]
    pub current_jobs: i32,
    #[serde(default)]
    pub advertised_max_concurrent: Option<i32>,
    #[serde(default)]
    pub hardware: Option<WorkerHardwarePublic>,
    #[serde(default)]
    pub agents: Vec<WorkerAgentRuntime>,
    /// This box is the API process (same hostname as the plane).
    #[serde(default)]
    pub is_control_plane_box: bool,
    /// The browser talking to the API is this box (loopback, or matching IP).
    #[serde(default)]
    pub is_this_machine: bool,
    #[serde(default)]
    pub install_request: Option<serde_json::Value>,
    #[serde(default)]
    pub runtime_settings: Option<serde_json::Value>,
    #[serde(default)]
    pub runtime_reported: Option<serde_json::Value>,
    /// Who may send work here. Always present on the card.
    #[serde(default = "default_send_policy")]
    pub send_policy: String,
    /// Pairer. Used with `send_policy` (only me).
    #[serde(default)]
    pub created_by: Option<Uuid>,
    #[serde(default = "default_run_mode")]
    pub run_mode: String,
}

/// Official CLI on a runner. Never includes tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkerAgentRuntime {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub models: Vec<String>,
    /// Per-model extras from the official CLI (effort menus, …).
    #[serde(default)]
    pub model_details: Vec<WorkerAgentModelDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkerAgentModelDetail {
    pub name: String,
    #[serde(default)]
    pub efforts: Vec<WorkerAgentEffort>,
    #[serde(default)]
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkerAgentEffort {
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
}

fn default_agent_runtimes() -> serde_json::Value {
    serde_json::json!([])
}

fn parse_agent_runtimes(v: &serde_json::Value) -> Vec<WorkerAgentRuntime> {
    let Some(arr) = v.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|item| serde_json::from_value(item.clone()).ok())
        .filter(|a: &WorkerAgentRuntime| !a.id.trim().is_empty())
        .collect()
}

fn default_auto_update() -> bool {
    true
}

fn default_isolation() -> String {
    WORKER_HOST_ISOLATION_NATIVE.to_string()
}

fn default_send_policy() -> String {
    WORKER_HOST_SEND_OWNER_ONLY.to_string()
}

fn default_run_mode() -> String {
    WORKER_HOST_RUN_ON_PC.to_string()
}

fn default_host_kind() -> String {
    WORKER_HOST_KIND_HOST.to_string()
}

fn default_software_status() -> String {
    WORKER_SOFTWARE_STATUS_UNKNOWN.to_string()
}

/// What the box last reported. `None` on `WorkerHostPublic.software` means never reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSoftwarePublic {
    pub git_sha: Option<String>,
    #[serde(default)]
    pub bins: Option<serde_json::Value>,
    pub reported_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updating: bool,
}

/// One binary in a staged platform package (`manifest.json` `bins` entry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerBinMeta {
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    /// Operator-facing package version from runner-packages.toml. Same on every OS.
    #[serde(default)]
    pub version: Option<String>,
}

/// Served dist for one platform. Source of truth for “what should be on the box”.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSoftwareManifest {
    #[serde(default)]
    pub git_sha: String,
    #[serde(default)]
    pub platform: String,
    /// When this package was last written. Empty if unknown.
    #[serde(default)]
    pub staged_at: String,
    #[serde(default)]
    pub bins: BTreeMap<String, WorkerBinMeta>,
}

/// Snapshot from the box. Not a monitoring series.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerHardwarePublic {
    #[serde(default)]
    pub cpu_cores: Option<u32>,
    #[serde(default)]
    pub mem_total_bytes: Option<u64>,
    #[serde(default)]
    pub mem_used_pct: Option<u8>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub cpu_used_pct: Option<u8>,
    #[serde(default)]
    pub mem_speed_mts: Option<u32>,
    #[serde(default)]
    pub mem_type: Option<String>,
    #[serde(default)]
    pub gpus: Option<Vec<WorkerGpuPublic>>,
    #[serde(default)]
    pub gpu_used_pct: Option<u8>,
    #[serde(default)]
    pub disk_total_bytes: Option<u64>,
    #[serde(default)]
    pub disk_free_bytes: Option<u64>,
    /// Non-loopback IPs this box advertised (for “this machine” matching).
    #[serde(default)]
    pub addrs: Option<Vec<String>>,
}

/// One GPU the box could name. `vram_bytes` omitted when unknown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerGpuPublic {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vram_bytes: Option<u64>,
    #[serde(default)]
    pub used_pct: Option<u8>,
}

fn enrich_reported_gpus(hw: &mut WorkerHardwarePublic) {
    if let Some(list) = hw.gpus.as_mut() {
        for g in list.iter_mut() {
            if let Some(n) = g.name.take() {
                let cleaned = super::normalize_gpu_name(&n);
                g.name = if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                };
            }
        }
        list.retain(|g| g.name.as_deref().is_some_and(|n| !n.trim().is_empty()));
        if !list.is_empty() {
            return;
        }
    }
    if let Some(name) = hw.cpu_model.as_deref().and_then(super::infer_igpu_from_cpu) {
        hw.gpus = Some(vec![WorkerGpuPublic {
            name: Some(name),
            vram_bytes: None,
            used_pct: None,
        }]);
    }
}

impl WorkerHostPublic {
    pub fn from_host(host: &WorkerHost, now: DateTime<Utc>) -> Self {
        Self {
            id: host.id,
            workspace_id: host.workspace_id,
            name: host.name.clone(),
            kind: WORKER_HOST_KIND_HOST.to_string(),
            status: host.public_status(now).to_string(),
            enabled: host.enabled,
            paused: host.paused,
            drain: host.drain,
            isolation: if host.isolation == WORKER_HOST_ISOLATION_SANDBOXED {
                WORKER_HOST_ISOLATION_SANDBOXED.to_string()
            } else {
                WORKER_HOST_ISOLATION_NATIVE.to_string()
            },
            allow_host: host.allow_host,
            allow_container: host.allow_container,
            max_concurrent: host.max_concurrent,
            max_job_timeout_s: host.max_job_timeout_s,
            labels: host.labels.clone(),
            advertised_host: host.advertised_host,
            advertised_container: host.advertised_container,
            platform: host.platform.clone(),
            hostname: host.hostname.clone(),
            last_seen_at: host.last_seen_at,
            last_error: host.last_error.clone(),
            created_at: host.created_at,
            paired_at: host.paired_at,
            software: if host.software_reported_at.is_some() || host.software_git_sha.is_some() {
                Some(WorkerSoftwarePublic {
                    git_sha: host.software_git_sha.clone(),
                    bins: host.software_bins.clone(),
                    reported_at: host.software_reported_at,
                    updating: host.software_updating,
                })
            } else {
                None
            },
            desired: None,
            software_status: WORKER_SOFTWARE_STATUS_UNKNOWN.to_string(),
            outdated: false,
            auto_update: host.auto_update,
            update_requested_at: host.update_requested_at,
            machine_id: host.machine_id,
            settings_gen: host.settings_gen,
            settings_ack_gen: host.settings_ack_gen,
            settings_pending: host.settings_pending(),
            current_jobs: host.current_jobs,
            advertised_max_concurrent: host.advertised_max_concurrent,
            hardware: host.hardware.as_ref().and_then(|v| {
                let mut hw: WorkerHardwarePublic = serde_json::from_value(v.clone()).ok()?;
                enrich_reported_gpus(&mut hw);
                Some(hw)
            }),
            agents: parse_agent_runtimes(&host.agent_runtimes),
            is_control_plane_box: false,
            is_this_machine: false,
            install_request: host.install_request.clone(),
            runtime_settings: Some(host.runtime_settings.clone())
                .filter(|v| v.as_object().is_some_and(|o| !o.is_empty())),
            runtime_reported: Some(host.runtime_reported.clone())
                .filter(|v| v.as_object().is_some_and(|o| !o.is_empty())),
            send_policy: if parse_worker_host_send_policy(&host.send_policy).is_some() {
                host.send_policy.clone()
            } else {
                WORKER_HOST_SEND_OWNER_ONLY.to_string()
            },
            created_by: host.created_by,
            run_mode: if parse_worker_host_run_mode(&host.run_mode).is_some() {
                host.run_mode.clone()
            } else {
                WORKER_HOST_RUN_ON_PC.to_string()
            },
        }
    }

    /// Attach the staged platform package and recompute `software_status` / `outdated`.
    pub fn with_desired(mut self, desired: Option<WorkerSoftwareManifest>) -> Self {
        let status = compute_software_status(self.software.as_ref(), desired.as_ref());
        self.desired = desired;
        self.software_status = status.to_string();
        self.outdated = status == WORKER_SOFTWARE_STATUS_BEHIND;
        self
    }
}

const PACKAGE_BIN_STEMS: [&str; 2] = ["fleet-host", "fleet-driver"];

/// `current` / `behind` / `unknown` from the box report vs the served dist.
///
/// No report or no manifest → `unknown` (not behind). Present package bins
/// that disagree → `behind`. Mixed match/mismatch is behind. When no bin
/// pair exists, fall back to both git shas only if they are bake ids (not
/// 64-hex file hashes). A file hash is integrity for bins, never a bake id.
pub fn compute_software_status(
    software: Option<&WorkerSoftwarePublic>,
    desired: Option<&WorkerSoftwareManifest>,
) -> &'static str {
    let Some(sw) = software else {
        return WORKER_SOFTWARE_STATUS_UNKNOWN;
    };
    let Some(desired) = desired else {
        return WORKER_SOFTWARE_STATUS_UNKNOWN;
    };

    let mut compared = 0u32;
    let mut mismatch = false;
    if let Some(bins) = sw.bins.as_ref() {
        for stem in PACKAGE_BIN_STEMS {
            let reported_ver = reported_bin_version(bins, stem);
            let want_ver = desired_bin_version(desired, stem);
            let reported = reported_bin_sha(bins, stem);
            let want = desired_bin_sha(desired, stem);
            if reported_ver.is_some() && want_ver.is_some() {
                compared += 1;
                if !sha_eq(
                    reported_ver.as_deref().unwrap(),
                    want_ver.as_deref().unwrap(),
                ) {
                    mismatch = true;
                }
                continue;
            }
            match (reported, want) {
                (Some(got), Some(expect)) => {
                    compared += 1;
                    if !sha_eq(&got, &expect) {
                        mismatch = true;
                    }
                }
                (None, Some(_)) => {
                    compared += 1;
                    mismatch = true;
                }
                _ => {}
            }
        }
    }
    if compared > 0 {
        return if mismatch {
            WORKER_SOFTWARE_STATUS_BEHIND
        } else {
            WORKER_SOFTWARE_STATUS_CURRENT
        };
    }

    // Served package lists fleet-host + fleet-driver. Do not call the box
    // current from bake id alone — both files have to be checked.
    if PACKAGE_BIN_STEMS
        .iter()
        .any(|stem| desired_bin_sha(desired, stem).is_some())
    {
        return WORKER_SOFTWARE_STATUS_BEHIND;
    }

    let reported_sha = sw
        .git_sha
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let desired_sha = desired.git_sha.trim();
    match (reported_sha, desired_sha.is_empty()) {
        (Some(got), false) => {
            // A 64-hex on either side is a file hash, not a bake id.
            if is_file_hash(got) || is_file_hash(desired_sha) {
                WORKER_SOFTWARE_STATUS_UNKNOWN
            } else if sha_eq(got, desired_sha) {
                WORKER_SOFTWARE_STATUS_CURRENT
            } else {
                WORKER_SOFTWARE_STATUS_BEHIND
            }
        }
        _ => WORKER_SOFTWARE_STATUS_UNKNOWN,
    }
}

/// SHA-256 of a bin on disk — never a tree-id / bake stamp.
fn is_file_hash(s: &str) -> bool {
    let s = s.trim();
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn bin_sha_from_value(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        return Some(s.to_string());
    }
    let s = v.get("sha256")?.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn lookup_bin_value<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    stem: &str,
) -> Option<&'a serde_json::Value> {
    let exe = format!("{stem}.exe");
    obj.get(stem).or_else(|| obj.get(exe.as_str()))
}

fn reported_bin_sha(bins: &serde_json::Value, stem: &str) -> Option<String> {
    let obj = bins.as_object()?;
    lookup_bin_value(obj, stem).and_then(bin_sha_from_value)
}

fn reported_bin_version(bins: &serde_json::Value, stem: &str) -> Option<String> {
    let obj = bins.as_object()?;
    let exe = format!("{stem}.exe");
    let v = obj.get(stem).or_else(|| obj.get(exe.as_str()))?;
    let s = v.get("version")?.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn desired_bin_version(desired: &WorkerSoftwareManifest, stem: &str) -> Option<String> {
    let exe = format!("{stem}.exe");
    desired
        .bins
        .get(stem)
        .or_else(|| desired.bins.get(exe.as_str()))
        .and_then(|b| b.version.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn desired_bin_sha(desired: &WorkerSoftwareManifest, stem: &str) -> Option<String> {
    let exe = format!("{stem}.exe");
    desired
        .bins
        .get(stem)
        .or_else(|| desired.bins.get(exe.as_str()))
        .map(|b| b.sha256.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Insert a pending host (pair token is a separate row).
#[derive(Debug, Clone)]
pub struct NewWorkerHost {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub created_by: Option<Uuid>,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
    pub send_policy: String,
    pub run_mode: String,
}

impl NewWorkerHost {
    pub fn new(
        id: Uuid,
        workspace_id: Uuid,
        name: impl Into<String>,
        created_by: Option<Uuid>,
    ) -> Self {
        Self {
            id,
            workspace_id,
            name: name.into(),
            created_by,
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            max_job_timeout_s: 3600,
            labels: Vec::new(),
            send_policy: WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            run_mode: WORKER_HOST_RUN_ON_PC.to_string(),
        }
    }
}

/// Partial CP settings (`PATCH`). `None` leaves the column unchanged.
#[derive(Debug, Clone, Default)]
pub struct HostCpSettings {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub drain: Option<bool>,
    /// `native` or `sandboxed`. `None` leaves the column unchanged.
    pub isolation: Option<String>,
    pub allow_host: Option<bool>,
    pub allow_container: Option<bool>,
    pub max_concurrent: Option<i32>,
    pub max_job_timeout_s: Option<i32>,
    pub labels: Option<Vec<String>>,
    pub auto_update: Option<bool>,
    /// When true, set `update_requested_at = now()` (host applies once even if auto is off).
    pub request_update: Option<bool>,
    /// Queue `ollama pull <spec>` (name or URL) on the next heartbeat.
    pub install_model: Option<serde_json::Value>,
    /// Merge into `runtime_settings` and bump settings_gen so the box applies it.
    pub runtime_settings: Option<serde_json::Value>,
}

#[cfg(test)]
mod public_view_tests {
    use super::*;

    fn host(status: &str, last_seen: Option<DateTime<Utc>>) -> WorkerHost {
        let now = Utc::now();
        WorkerHost {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            name: "studio".into(),
            status: status.into(),
            enabled: true,
            paused: false,
            drain: false,
            isolation: WORKER_HOST_ISOLATION_NATIVE.into(),
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: Some("should-never-leave".into()),
            advertised_host: true,
            advertised_container: false,
            platform: Some("linux-x86_64".into()),
            hostname: Some("box".into()),
            last_seen_at: last_seen,
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
            send_policy: WORKER_HOST_SEND_OWNER_ONLY.to_string(),
            run_mode: WORKER_HOST_RUN_ON_PC.to_string(),
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn online_host_stays_online_through_missed_heartbeat() {
        let now = Utc::now();
        let missed = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(60)));
        assert_eq!(
            missed.public_status(now),
            WORKER_HOST_STATUS_ONLINE,
            "a missed beat must not flip the card offline"
        );
    }

    #[test]
    fn online_at_exactly_offline_after_stays_online() {
        let now = Utc::now();
        let at_threshold = host(
            WORKER_HOST_STATUS_ONLINE,
            Some(now - WORKER_HOST_OFFLINE_AFTER),
        );
        assert_eq!(at_threshold.public_status(now), WORKER_HOST_STATUS_ONLINE);
    }

    #[test]
    fn stale_online_host_is_reported_offline() {
        let now = Utc::now();
        let stale = host(
            WORKER_HOST_STATUS_ONLINE,
            Some(now - WORKER_HOST_OFFLINE_AFTER - Duration::seconds(1)),
        );
        assert_eq!(stale.public_status(now), "offline");
    }

    #[test]
    fn online_host_without_last_seen_is_reported_offline() {
        let now = Utc::now();
        let never_seen = host(WORKER_HOST_STATUS_ONLINE, None);
        assert_eq!(never_seen.public_status(now), "offline");
    }

    #[test]
    fn public_hardware_infers_intel_uhd_for_x1_cpu() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.hardware = Some(serde_json::json!({
            "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
            "cpu_cores": 8
        }));
        let pub_h = h.as_public(Utc::now());
        let gpus = pub_h.hardware.expect("hw").gpus.expect("gpus");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name.as_deref(), Some("Intel UHD Graphics"));
    }

    #[test]
    fn cluster_label_is_trimmed_case_insensitive() {
        assert!(label_is_cluster("cluster"));
        assert!(label_is_cluster("CLUSTER"));
        assert!(label_is_cluster(" Cluster "));
        assert!(label_is_cluster("cLuStEr"));
        assert!(!label_is_cluster("gpu"));
        assert!(!label_is_cluster("cluster-host"));
        assert!(!label_is_cluster(""));
    }

    #[test]
    fn is_cluster_host_true_when_any_label_is_cluster() {
        let mut boxed = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        boxed.labels = vec!["gpu".into(), " CLUSTER ".into()];
        assert!(
            boxed.is_cluster_host(),
            "cluster anywhere in labels is a cluster host"
        );
    }

    #[test]
    fn normalize_host_labels_preserves_non_cluster_case() {
        assert_eq!(
            normalize_host_labels(&["GPU".into(), "Linux".into()]),
            vec!["GPU".to_string(), "Linux".to_string()]
        );
    }

    #[test]
    fn parse_worker_host_isolation_only_native_or_sandboxed() {
        assert_eq!(
            parse_worker_host_isolation("native"),
            Some(WORKER_HOST_ISOLATION_NATIVE)
        );
        assert_eq!(
            parse_worker_host_isolation(" sandboxed "),
            Some(WORKER_HOST_ISOLATION_SANDBOXED)
        );
        assert_eq!(parse_worker_host_isolation("k8s"), None);
        assert_eq!(parse_worker_host_isolation(""), None);
    }

    #[test]
    fn parse_send_policy_wire_values() {
        assert_eq!(
            parse_worker_host_send_policy("owner_only"),
            Some(WORKER_HOST_SEND_OWNER_ONLY)
        );
        assert_eq!(
            parse_worker_host_send_policy("allowlist"),
            Some(WORKER_HOST_SEND_ALLOWLIST)
        );
        assert_eq!(
            parse_worker_host_send_policy("workspace"),
            Some(WORKER_HOST_SEND_WORKSPACE)
        );
        assert_eq!(parse_worker_host_send_policy("shared"), None);
        assert_eq!(parse_worker_host_send_policy("OWNER_ONLY"), None);
    }

    #[test]
    fn parse_worker_host_isolation_rejects_case_and_whitespace_only() {
        assert_eq!(parse_worker_host_isolation("NATIVE"), None);
        assert_eq!(parse_worker_host_isolation("Sandboxed"), None);
        assert_eq!(parse_worker_host_isolation("   "), None);
    }

    #[test]
    fn as_public_sandboxed_isolation_stays_sandboxed() {
        let mut boxed = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        boxed.isolation = WORKER_HOST_ISOLATION_SANDBOXED.into();
        assert!(!boxed.is_native_isolation());
        assert_eq!(
            boxed.as_public(Utc::now()).isolation,
            WORKER_HOST_ISOLATION_SANDBOXED
        );
    }

    #[test]
    fn as_public_unknown_isolation_is_native() {
        let now = Utc::now();
        let mut boxed = host(WORKER_HOST_STATUS_ONLINE, Some(now));
        boxed.isolation = "k8s".into();
        assert_eq!(boxed.as_public(now).isolation, WORKER_HOST_ISOLATION_NATIVE);
        boxed.isolation = "cloud".into();
        assert_eq!(boxed.as_public(now).isolation, WORKER_HOST_ISOLATION_NATIVE);
        boxed.isolation = "".into();
        assert_eq!(boxed.as_public(now).isolation, WORKER_HOST_ISOLATION_NATIVE);
    }

    #[test]
    fn as_public_agents_maps_model_details() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.agent_runtimes = serde_json::json!([
            {
                "id": "grok",
                "name": "Grok",
                "installed": true,
                "logged_in": true,
                "models": ["grok-4"],
                "model_details": [{
                    "name": "grok-4",
                    "efforts": [{"value": "high", "label": "High"}],
                    "default_effort": "high"
                }]
            },
            {
                "id": "claude",
                "name": "Claude",
                "installed": true,
                "logged_in": false,
                "models": [],
                "model_details": []
            }
        ]);
        assert_eq!(
            h.as_public(Utc::now()).agents,
            vec![
                WorkerAgentRuntime {
                    id: "grok".into(),
                    name: Some("Grok".into()),
                    installed: true,
                    logged_in: true,
                    models: vec!["grok-4".into()],
                    model_details: vec![WorkerAgentModelDetail {
                        name: "grok-4".into(),
                        efforts: vec![WorkerAgentEffort {
                            value: "high".into(),
                            label: Some("High".into()),
                        }],
                        default_effort: Some("high".into()),
                    }],
                },
                WorkerAgentRuntime {
                    id: "claude".into(),
                    name: Some("Claude".into()),
                    installed: true,
                    logged_in: false,
                    models: vec![],
                    model_details: vec![],
                },
            ]
        );
    }

    #[test]
    fn as_public_agents_drops_blank_ids_and_non_objects() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.agent_runtimes = serde_json::json!([
            {"id": ""},
            {"id": "  "},
            {},
            "x",
            {"id": "grok"}
        ]);
        let agents = h.as_public(Utc::now()).agents;
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["grok"]
        );
    }

    #[test]
    fn as_public_agents_empty_when_runtimes_not_an_array() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.agent_runtimes = serde_json::json!({"id": "grok"});
        assert!(
            h.as_public(Utc::now()).agents.is_empty(),
            "non-array agent_runtimes must surface as no agents"
        );
    }

    #[test]
    fn pending_and_disabled_are_not_rewritten_to_offline() {
        let now = Utc::now();
        let pending = host(WORKER_HOST_STATUS_PENDING, None);
        assert_eq!(pending.public_status(now), WORKER_HOST_STATUS_PENDING);
        let disabled = host(WORKER_HOST_STATUS_DISABLED, Some(now - Duration::hours(1)));
        assert_eq!(disabled.public_status(now), WORKER_HOST_STATUS_DISABLED);
    }

    #[test]
    fn public_json_never_includes_hashes_or_tokens() {
        let h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        let public = serde_json::to_string(&h.as_public(Utc::now())).expect("json");
        let raw = serde_json::to_string(&h).expect("host json");
        for (label, blob) in [("as_public", &public), ("WorkerHost", &raw)] {
            for secret in [
                "machine_token_hash",
                "pair_token",
                "pair_token_hash",
                "token_hash",
                "machine_token",
                "should-never-leave",
            ] {
                assert!(
                    !blob.contains(secret),
                    "{label} leaked {secret}: {blob}"
                );
            }
        }
    }

    #[test]
    fn from_host_leaves_desired_none_when_software_is_reported() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.software_git_sha = Some("reported-sha".into());
        h.software_reported_at = Some(Utc::now());
        let public = h.as_public(Utc::now());
        assert!(
            public.software.is_some(),
            "reported software must appear so desired-none is not the never-reported path"
        );
        assert!(
            public.desired.is_none(),
            "from_host must leave desired unset; host_json attaches via with_desired"
        );
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "from_host hardcodes unknown; with_desired recomputes against a staged manifest"
        );
    }

    fn sample_manifest(host_sha: &str) -> WorkerSoftwareManifest {
        let mut bins = BTreeMap::new();
        bins.insert(
            "fleet-host".into(),
            WorkerBinMeta {
                sha256: host_sha.into(),
                size: 10,
                version: Some("0.0.1".into()),
            },
        );
        bins.insert(
            "fleet-driver".into(),
            WorkerBinMeta {
                sha256: "driver-sha".into(),
                size: 12,
                version: Some("0.0.1".into()),
            },
        );
        WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins,
        }
    }

    fn reported(git_sha: Option<&str>, bins: Option<serde_json::Value>) -> WorkerSoftwarePublic {
        WorkerSoftwarePublic {
            git_sha: git_sha.map(str::to_string),
            bins,
            reported_at: Some(Utc::now()),
            updating: false,
        }
    }

    #[test]
    fn software_status_unknown_without_report_or_manifest() {
        let sw = reported(
            Some("stage-tree-id"),
            Some(serde_json::json!({
                "fleet-host": "aaa",
                "fleet-driver": "driver-sha"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), None),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "no staged manifest is unknown, not behind, even when host+driver bins were reported"
        );
    }

    #[test]
    fn with_desired_none_on_reported_host_is_unknown_not_outdated() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.software_git_sha = Some("stage-tree-id".into());
        h.software_bins = Some(serde_json::json!({
            "fleet-host": "aaa",
            "fleet-driver": "driver-sha"
        }));
        h.software_reported_at = Some(Utc::now());
        let public = h.as_public(Utc::now()).with_desired(None);
        assert!(
            public.software.is_some(),
            "reported software must appear so unknown is not the never-reported path"
        );
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "desired_for withhold or no staged platform must not flag Update"
        );
        assert!(
            !public.outdated,
            "unknown is never outdated"
        );
    }

    #[test]
    fn software_status_unknown_when_manifest_has_empty_git_sha_and_no_bins() {
        let desired = WorkerSoftwareManifest {
            git_sha: String::new(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let sw = reported(Some("stage-tree-id"), None);
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "empty bake id and no package bins cannot be behind"
        );
    }

    #[test]
    fn software_status_behind_when_present_bin_mismatches() {
        let desired = sample_manifest("aaa");
        let sw = reported(
            Some("stage-tree-id"),
            Some(serde_json::json!({
                "fleet-host": "bbb",
                "fleet-driver": "driver-sha"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND
        );
    }

    #[test]
    fn missing_fleet_driver_is_behind() {
        let desired = sample_manifest("aaa");
        let sw = reported(
            Some("other-git"),
            Some(serde_json::json!({
                "fleet-host": "aaa"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "SKU is fleet-host + fleet-driver; a box that does not report the guest is behind"
        );
    }

    #[test]
    fn software_status_current_when_bins_match() {
        let desired = sample_manifest("AaA");
        let sw = reported(
            Some("other-git"),
            Some(serde_json::json!({
                "fleet-host": "aaa",
                "fleet-driver": "driver-sha"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT
        );
    }

    #[test]
    fn software_status_behind_when_old_package_version_matches_hash() {
        let desired = sample_manifest("aaa");
        let old_ver = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": { "sha256": "aaa", "version": "0.0.0" },
                "fleet-driver": { "sha256": "driver-sha", "version": "0.0.1" }
            })),
        );
        assert_eq!(
            compute_software_status(Some(&old_ver), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "host 0.0.0 vs staged 0.0.1 is behind even when sha256 matches"
        );
    }

    #[test]
    fn software_status_uses_package_version_then_hash() {
        let desired = sample_manifest("aaa");
        let same_ver_wrong_hash = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": { "sha256": "not-aaa", "version": "0.0.1" },
                "fleet-driver": { "sha256": "driver-sha", "version": "0.0.1" }
            })),
        );
        assert_eq!(
            compute_software_status(Some(&same_ver_wrong_hash), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "same 0.0.1 with a hash mismatch is current; version compare skips hash"
        );
    }

    #[test]
    fn software_status_behind_when_only_driver_version_mismatches() {
        let desired = sample_manifest("aaa");
        let old_driver = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": { "sha256": "aaa", "version": "0.0.1" },
                "fleet-driver": { "sha256": "driver-sha", "version": "0.0.0" }
            })),
        );
        assert_eq!(
            compute_software_status(Some(&old_driver), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "driver 0.0.0 vs staged 0.0.1 is behind even when host version and hashes match"
        );
    }

    #[test]
    fn software_status_behind_on_mixed_bins() {
        let desired = sample_manifest("aaa");
        let sw = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": "aaa",
                "fleet-driver": "wrong"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND
        );
    }

    #[test]
    fn software_status_maps_windows_exe_keys() {
        let mut bins = BTreeMap::new();
        bins.insert(
            "fleet-host.exe".into(),
            WorkerBinMeta {
                sha256: "pe-host".into(),
                size: 1,
                version: None,
            },
        );
        bins.insert(
            "fleet-driver.exe".into(),
            WorkerBinMeta {
                sha256: "pe-driver".into(),
                size: 1,
                version: None,
            },
        );
        let desired = WorkerSoftwareManifest {
            git_sha: "win".into(),
            platform: "windows-x86_64".into(),
            staged_at: String::new(),
            bins,
        };
        let sw = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": "pe-host",
                "fleet-driver": "pe-driver"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT
        );
    }

    #[test]
    fn software_status_requires_both_bins_not_bake_id_alone() {
        let desired = sample_manifest("aaa");
        let bake_only = reported(Some("stage-tree-id"), None);
        assert_eq!(
            compute_software_status(Some(&bake_only), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "git_sha match is not enough when the package lists host + driver"
        );
    }

    #[test]
    fn empty_reported_bins_object_matching_bake_is_behind() {
        let desired = sample_manifest("aaa");
        let empty_bins = reported(Some("stage-tree-id"), Some(serde_json::json!({})));
        assert_eq!(
            compute_software_status(Some(&empty_bins), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "empty bins object is not a host+driver pair; matching bake must stay behind"
        );
    }

    #[test]
    fn with_desired_matching_bake_without_bins_is_outdated() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.software_git_sha = Some("stage-tree-id".into());
        h.software_reported_at = Some(Utc::now());
        let public = h
            .as_public(Utc::now())
            .with_desired(Some(sample_manifest("aaa")));
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_BEHIND,
            "matching bake without reported bins is behind when the package lists host+driver"
        );
        assert!(
            public.outdated,
            "behind from bake-id-alone must set outdated so Update now is shown"
        );
    }

    #[test]
    fn software_status_falls_back_to_git_sha_when_package_has_no_bins() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let current = reported(Some("stage-tree-id"), None);
        assert_eq!(
            compute_software_status(Some(&current), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "matching bake id is current when the staged package has no bins"
        );
    }

    #[test]
    fn software_status_behind_when_git_sha_mismatches_and_package_has_no_bins() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let behind = reported(Some("old-tree"), None);
        assert_eq!(
            compute_software_status(Some(&behind), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "mismatched bake id is behind when the staged package has no bins"
        );
    }

    #[test]
    fn software_status_unknown_when_reported_git_sha_is_whitespace_and_package_has_no_bins() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let empty = reported(Some("  "), None);
        assert_eq!(
            compute_software_status(Some(&empty), Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "whitespace reported git_sha is treated as missing; empty-bin fallback is unknown"
        );
    }

    #[test]
    fn software_status_git_sha_fallback_ignores_case_and_trim() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let sw = reported(Some(" Stage-Tree-Id "), None);
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "empty-bin bake-id fallback uses sha_eq (trim + ignore case)"
        );
    }

    #[test]
    fn software_status_unknown_when_report_omits_git_sha_and_package_has_no_bins() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let sw = reported(None, None);
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "omitted reported git_sha cannot compare bake ids; empty-bin fallback is unknown"
        );
    }

    #[test]
    fn software_status_empty_reported_bins_object_still_falls_back() {
        let desired = WorkerSoftwareManifest {
            git_sha: "stage-tree-id".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let sw = reported(Some("stage-tree-id"), Some(serde_json::json!({})));
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "empty reported bins object still leaves compared==0 so empty-package bake-id fallback runs"
        );
    }

    #[test]
    fn compute_software_status_never_treats_file_hash_as_bake_id() {
        let file_hash = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e";
        let other_hash = "6bec5b00654e94f1a469f1f7cee6f939413b6e36a942b444e798439fb642309d";
        let bake = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351";

        fn empty_bins(git_sha: &str) -> WorkerSoftwareManifest {
            WorkerSoftwareManifest {
                git_sha: git_sha.into(),
                platform: "linux-x86_64".into(),
                staged_at: String::new(),
                bins: BTreeMap::new(),
            }
        }

        // Empty-bin fallback: a 64-hex on either side is not a bake id.
        let both_hash = reported(Some(file_hash), None);
        assert_eq!(
            compute_software_status(Some(&both_hash), Some(&empty_bins(file_hash))),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "matching 64-hex git_sha is not a bake-id compare"
        );
        assert_eq!(
            compute_software_status(
                Some(&reported(Some(file_hash), None)),
                Some(&empty_bins(bake))
            ),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "reported 64-hex git_sha is not a bake id"
        );
        assert_eq!(
            compute_software_status(
                Some(&reported(Some(bake), None)),
                Some(&empty_bins(file_hash))
            ),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "manifest 64-hex git_sha is not a bake id"
        );
        assert_eq!(
            compute_software_status(
                Some(&reported(Some(file_hash), None)),
                Some(&empty_bins(other_hash))
            ),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "two different 64-hex git_sha values are still file hashes, not bake ids"
        );
    }

    #[test]
    fn software_status_current_when_64_hex_bin_hashes_match() {
        let file_hash = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e";
        let other_hash = "6bec5b00654e94f1a469f1f7cee6f939413b6e36a942b444e798439fb642309d";
        let mut hash_bins = BTreeMap::new();
        hash_bins.insert(
            "fleet-host".into(),
            WorkerBinMeta {
                sha256: file_hash.into(),
                size: 1,
                version: None,
            },
        );
        hash_bins.insert(
            "fleet-driver".into(),
            WorkerBinMeta {
                sha256: other_hash.into(),
                size: 1,
                version: None,
            },
        );
        let desired = WorkerSoftwareManifest {
            git_sha: "18a1be7df0a53136e7a85b741d8b9fc45b4cc351".into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: hash_bins,
        };
        let sw = reported(
            Some(file_hash),
            Some(serde_json::json!({
                "fleet-host": file_hash,
                "fleet-driver": other_hash
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "matching 64-hex bin hashes are integrity; short-hash CURRENT siblings skip this path"
        );
    }

    #[test]
    fn software_status_40_hex_tree_id_empty_bins_compares_as_bake_id() {
        let tree = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351";
        let other = "6bec5b00654e94f1a469f1f7cee6f939413b6e36";
        let desired = WorkerSoftwareManifest {
            git_sha: tree.into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let current = reported(Some(tree), None);
        assert_eq!(
            compute_software_status(Some(&current), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT,
            "40-hex tree-id is a bake id; matching empty-bin report is current"
        );
        let behind = reported(Some(other), None);
        assert_eq!(
            compute_software_status(Some(&behind), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND,
            "two different 40-hex tree-ids compare as bake ids, not unknown"
        );
    }

    #[test]
    fn software_status_uppercase_64_hex_git_sha_is_unknown() {
        let file_hash = "2A971D693B6F7F0C13C94269845CA4AC0C73672B72AC7A2C7839BE2CEE590C8E";
        let desired = WorkerSoftwareManifest {
            git_sha: file_hash.into(),
            platform: "linux-x86_64".into(),
            staged_at: String::new(),
            bins: BTreeMap::new(),
        };
        let sw = reported(Some(file_hash), None);
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "uppercase 64-hex git_sha is a file hash, not a bake id"
        );
    }

    #[test]
    fn with_desired_sets_outdated_only_when_behind() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.software_git_sha = Some("old".into());
        h.software_reported_at = Some(Utc::now());
        let public = h
            .as_public(Utc::now())
            .with_desired(Some(sample_manifest("aaa")));
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_BEHIND,
            "bake-only report against a staged host+driver package is behind"
        );
        assert!(
            public.outdated,
            "outdated is true only when software_status is behind"
        );
    }

    #[test]
    fn with_desired_never_reported_host_is_unknown_not_outdated() {
        let public = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()))
            .as_public(Utc::now())
            .with_desired(Some(sample_manifest("aaa")));
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_UNKNOWN,
            "a never-reported box stays unknown even when a package is staged"
        );
        assert!(
            !public.outdated,
            "unknown must not light Update now on a never-reported box"
        );
    }

    #[test]
    fn with_desired_current_matching_bins_is_not_outdated() {
        let mut h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        h.software_git_sha = Some("stage-tree-id".into());
        h.software_bins = Some(serde_json::json!({
            "fleet-host": "aaa",
            "fleet-driver": "driver-sha"
        }));
        h.software_reported_at = Some(Utc::now());
        let public = h
            .as_public(Utc::now())
            .with_desired(Some(sample_manifest("aaa")));
        assert_eq!(
            public.software_status,
            WORKER_SOFTWARE_STATUS_CURRENT,
            "matching fleet-host+fleet-driver hashes are current"
        );
        assert!(
            !public.outdated,
            "current must not be outdated; outdated is not merely status != unknown"
        );
    }
}

/// Pair-token row (`worker_host_pair_tokens`). Store only `token_hash`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkerHostPairToken {
    pub id: Uuid,
    pub host_id: Uuid,
    pub workspace_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
