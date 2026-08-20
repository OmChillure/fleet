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
pub const WORKER_HOST_ISOLATION_NATIVE: &str = "native";
/// Placeholder self-hosted sandbox. Not assignable; no k8s executor in v1.
pub const WORKER_HOST_ISOLATION_SANDBOXED: &str = "sandboxed";

/// `native` or `sandboxed`. Other values are rejected.
pub fn parse_worker_host_isolation(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        WORKER_HOST_ISOLATION_NATIVE => Some(WORKER_HOST_ISOLATION_NATIVE),
        WORKER_HOST_ISOLATION_SANDBOXED => Some(WORKER_HOST_ISOLATION_SANDBOXED),
        _ => None,
    }
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
    pub settings_gen: i32,
    pub settings_ack_gen: i32,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub paired_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl WorkerHost {
    /// Plane ∩ advertised. Worker-local `run_*` is applied on the box, not here.
    pub fn effective_host(&self) -> bool {
        self.allow_host && self.advertised_host
    }

    pub fn effective_container(&self) -> bool {
        self.allow_container && self.advertised_container
    }

    /// Tick / lease assignment: only native isolation is v1.
    pub fn is_native_isolation(&self) -> bool {
        self.isolation == WORKER_HOST_ISOLATION_NATIVE
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
}

fn default_auto_update() -> bool {
    true
}

fn default_isolation() -> String {
    WORKER_HOST_ISOLATION_NATIVE.to_string()
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

const PACKAGE_BIN_STEMS: [&str; 3] = ["fleet-host", "fleet-worker", "fleet-driver"];

/// `current` / `behind` / `unknown` from the box report vs the served dist.
///
/// No report or no manifest → `unknown` (not behind). Present package bins
/// that disagree → `behind`. Mixed match/mismatch is behind. When no bin
/// pair exists, fall back to both git shas if they are non-empty.
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
            let reported = reported_bin_sha(bins, stem);
            let want = desired_bin_sha(desired, stem);
            match (reported, want) {
                (Some(got), Some(expect)) => {
                    compared += 1;
                    if !sha_eq(&got, &expect) {
                        mismatch = true;
                    }
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

    let reported_sha = sw
        .git_sha
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let desired_sha = desired.git_sha.trim();
    match (reported_sha, desired_sha.is_empty()) {
        (Some(got), false) => {
            if sha_eq(got, desired_sha) {
                WORKER_SOFTWARE_STATUS_CURRENT
            } else {
                WORKER_SOFTWARE_STATUS_BEHIND
            }
        }
        _ => WORKER_SOFTWARE_STATUS_UNKNOWN,
    }
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
            settings_gen: 0,
            settings_ack_gen: 0,
            created_by: None,
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn stale_online_host_is_reported_offline() {
        let now = Utc::now();
        let fresh = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(5)));
        assert_eq!(fresh.public_status(now), WORKER_HOST_STATUS_ONLINE);

        let paused = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(60)));
        assert_eq!(
            paused.public_status(now),
            WORKER_HOST_STATUS_ONLINE,
            "a missed beat must not flip the card offline"
        );

        let stale = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(91)));
        assert_eq!(stale.public_status(now), "offline");

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
    fn isolation_parse_and_public_view() {
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

        let mut boxed = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        boxed.isolation = WORKER_HOST_ISOLATION_SANDBOXED.into();
        assert!(!boxed.is_native_isolation());
        assert_eq!(
            boxed.as_public(Utc::now()).isolation,
            WORKER_HOST_ISOLATION_SANDBOXED
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
        let v = serde_json::to_value(h.as_public(Utc::now())).expect("json");
        let obj = v.as_object().expect("object");
        for secret in [
            "machine_token_hash",
            "pair_token",
            "pair_token_hash",
            "token_hash",
            "machine_token",
        ] {
            assert!(
                !obj.contains_key(secret),
                "public host leaked {secret}: {v}"
            );
        }
        // Stored WorkerHost must also skip the hash on accidental serialize.
        let raw = serde_json::to_value(&h).expect("host json");
        assert!(
            raw.get("machine_token_hash").is_none(),
            "WorkerHost serialize leaked hash: {raw}"
        );
        assert_eq!(v["status"], "online");
        assert_eq!(v["isolation"], WORKER_HOST_ISOLATION_NATIVE);
        assert!(h.is_native_isolation());
        assert!(
            v.get("software").unwrap().is_null(),
            "never-reported software must be null: {v}"
        );
        assert_eq!(v["software_status"], "unknown");
        assert_eq!(v["outdated"], false);
        assert!(
            v.get("desired").unwrap().is_null(),
            "desired is attached by the API, not from_host: {v}"
        );
    }

    fn sample_manifest(host_sha: &str) -> WorkerSoftwareManifest {
        let mut bins = BTreeMap::new();
        bins.insert(
            "fleet-host".into(),
            WorkerBinMeta {
                sha256: host_sha.into(),
                size: 10,
            },
        );
        bins.insert(
            "fleet-worker".into(),
            WorkerBinMeta {
                sha256: "worker-sha".into(),
                size: 11,
            },
        );
        bins.insert(
            "fleet-driver".into(),
            WorkerBinMeta {
                sha256: "driver-sha".into(),
                size: 12,
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
        let desired = sample_manifest("aaa");
        assert_eq!(
            compute_software_status(None, Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN
        );
        let sw = reported(Some("stage-tree-id"), None);
        assert_eq!(
            compute_software_status(Some(&sw), None),
            WORKER_SOFTWARE_STATUS_UNKNOWN
        );
    }

    #[test]
    fn software_status_behind_when_present_bin_mismatches() {
        let desired = sample_manifest("aaa");
        let sw = reported(
            Some("stage-tree-id"),
            Some(serde_json::json!({
                "fleet-host": "bbb",
                "fleet-worker": "worker-sha",
                "fleet-driver": "driver-sha"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND
        );
    }

    #[test]
    fn software_status_current_when_bins_match() {
        let desired = sample_manifest("AaA");
        let sw = reported(
            Some("other-git"),
            Some(serde_json::json!({
                "fleet-host": "aaa",
                "fleet-worker": { "sha256": "worker-sha", "size": 11 },
                "fleet-driver": "driver-sha"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT
        );
    }

    #[test]
    fn software_status_behind_on_mixed_bins() {
        let desired = sample_manifest("aaa");
        let sw = reported(
            None,
            Some(serde_json::json!({
                "fleet-host": "aaa",
                "fleet-worker": "wrong"
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
            },
        );
        bins.insert(
            "fleet-worker.exe".into(),
            WorkerBinMeta {
                sha256: "pe-worker".into(),
                size: 1,
            },
        );
        bins.insert(
            "fleet-driver.exe".into(),
            WorkerBinMeta {
                sha256: "pe-driver".into(),
                size: 1,
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
                "fleet-worker": "pe-worker",
                "fleet-driver": "pe-driver"
            })),
        );
        assert_eq!(
            compute_software_status(Some(&sw), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT
        );
    }

    #[test]
    fn software_status_falls_back_to_git_sha_when_bins_missing() {
        let desired = sample_manifest("aaa");
        let current = reported(Some("stage-tree-id"), None);
        assert_eq!(
            compute_software_status(Some(&current), Some(&desired)),
            WORKER_SOFTWARE_STATUS_CURRENT
        );
        let behind = reported(Some("old-tree"), Some(serde_json::json!({})));
        assert_eq!(
            compute_software_status(Some(&behind), Some(&desired)),
            WORKER_SOFTWARE_STATUS_BEHIND
        );
        let empty = reported(Some("  "), None);
        assert_eq!(
            compute_software_status(Some(&empty), Some(&desired)),
            WORKER_SOFTWARE_STATUS_UNKNOWN
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
        assert_eq!(public.software_status, WORKER_SOFTWARE_STATUS_BEHIND);
        assert!(public.outdated);
        assert_eq!(
            public.desired.as_ref().map(|d| d.git_sha.as_str()),
            Some("stage-tree-id")
        );

        let no_sw = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()))
            .as_public(Utc::now())
            .with_desired(Some(sample_manifest("aaa")));
        assert_eq!(no_sw.software_status, WORKER_SOFTWARE_STATUS_UNKNOWN);
        assert!(!no_sw.outdated);
        assert!(no_sw.desired.is_some());
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
