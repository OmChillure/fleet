//! Wire types for the self-hosted runners page (no HTTP).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostPublic {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
    pub status: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub drain: bool,
    #[serde(default)]
    pub isolation: Option<String>,
    #[serde(default = "default_true")]
    pub allow_host: bool,
    #[serde(default = "default_true")]
    pub allow_container: bool,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: i32,
    #[serde(default = "default_timeout")]
    pub max_job_timeout_s: i32,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub advertised_host: bool,
    #[serde(default)]
    pub advertised_container: bool,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub paired_at: Option<String>,
    #[serde(default)]
    pub software: Option<HostSoftware>,
    #[serde(default)]
    pub desired: Option<HostDesiredSoftware>,
    #[serde(default)]
    pub software_status: Option<String>,
    #[serde(default)]
    pub outdated: bool,
    /// Copied from the list payload. Used to tell "matches last restage"
    /// from "matches this control plane". Not sent by the host.
    #[serde(default, skip_serializing)]
    pub plane_git_sha: Option<String>,
    /// Copied from the list payload. Same number on every OS (see runner-packages.toml).
    #[serde(default, skip_serializing)]
    pub current_versions: PackageVersions,
    #[serde(default = "default_true")]
    pub auto_update: bool,
    #[serde(default)]
    pub update_requested_at: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
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
    pub hardware: Option<HostHardware>,
    /// Official agent CLIs last reported by this box. Never includes tokens.
    #[serde(default)]
    pub agents: Vec<AgentRuntime>,
    #[serde(default)]
    pub is_control_plane_box: bool,
    #[serde(default)]
    pub is_this_machine: bool,
    #[serde(default)]
    pub install_request: Option<serde_json::Value>,
    #[serde(default)]
    pub runtime_settings: Option<serde_json::Value>,
    #[serde(default)]
    pub runtime_reported: Option<serde_json::Value>,
}

/// One official agent CLI on a runner (`grok`, `claude`, `openai`, `ollama`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
pub struct AgentRuntime {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub model_details: Vec<AgentModelDetail>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
pub struct AgentModelDetail {
    pub name: String,
    #[serde(default)]
    pub efforts: Vec<AgentEffort>,
    #[serde(default)]
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
pub struct AgentEffort {
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct HostHardware {
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
    pub gpus: Option<Vec<HostGpu>>,
    #[serde(default)]
    pub gpu_used_pct: Option<u8>,
    #[serde(default)]
    pub disk_total_bytes: Option<u64>,
    #[serde(default)]
    pub disk_free_bytes: Option<u64>,
    #[serde(default)]
    pub addrs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct HostGpu {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vram_bytes: Option<u64>,
    #[serde(default)]
    pub used_pct: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostSoftware {
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub bins: Option<serde_json::Value>,
    #[serde(default)]
    pub updating: bool,
}

/// Staged dist for this host's platform (`GET …/manifest.json`).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct HostDesiredSoftware {
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub staged_at: Option<String>,
    #[serde(default)]
    pub bins: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct PackageVersions {
    #[serde(default, rename = "fleet-host")]
    pub host: String,
    #[serde(default, rename = "fleet-driver")]
    pub driver: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct PackageInventory {
    #[serde(default)]
    pub packages: Vec<HostDesiredSoftware>,
    #[serde(default)]
    pub plane_git_sha: Option<String>,
    #[serde(default)]
    pub package_versions: PackageVersions,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CreateHostResponse {
    pub host: HostPublic,
    pub pair_token: String,
    #[serde(default)]
    pub pair_expires_at: String,
    #[serde(default)]
    pub install_hint: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HostPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_host: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_container: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_model: Option<InstallModelRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_job_timeout_s: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct InstallModelRequest {
    #[serde(default)]
    pub provider: String,
    /// Model name or pull URL as the official CLI accepts it.
    #[serde(default)]
    pub spec: String,
}

fn default_true() -> bool {
    true
}

fn default_max_concurrent() -> i32 {
    4
}

fn default_timeout() -> i32 {
    3600
}
