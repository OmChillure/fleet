//! Sandbox domain commands and lifecycle events.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{SandboxId, SessionId};
use crate::types::{NetworkPolicy, ResourceSpec, SandboxBackend, SandboxNetworkInfo};

// --- commands ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverLaunchSpec {
    pub image_or_binary: String,
    /// How the Driver finds the control endpoint / session tokens (blob ref, not secrets).
    pub config_blob_ref: String,
    /// Repo / caches / agent package — see workspace provision.
    pub workspace_spec_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSandbox {
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<SessionId>,
    pub backend: SandboxBackend,
    pub image: String,
    pub resources: ResourceSpec,
    pub network_policy: NetworkPolicy,
    pub driver: DriverLaunchSpec,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_duration::option_secs"
    )]
    pub ttl: Option<Duration>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_duration::option_secs"
    )]
    pub idle_timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopSandbox {
    pub sandbox_id: SandboxId,
    pub reason: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_duration::option_secs"
    )]
    pub grace_period: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ArchiveRetain {
    #[serde(default)]
    pub volumes: bool,
    #[serde(default)]
    pub snapshot: bool,
    #[serde(default)]
    pub logs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveSandbox {
    pub sandbox_id: SandboxId,
    pub retain: ArchiveRetain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestroySandbox {
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecInSandbox {
    pub sandbox_id: SandboxId,
    pub command: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_duration::option_secs"
    )]
    pub timeout: Option<Duration>,
}

// --- events ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCreating {
    pub sandbox_id: SandboxId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxReady {
    pub sandbox_id: SandboxId,
    pub backend: SandboxBackend,
    /// Pod name / VM id / container name.
    pub backend_ref: String,
    /// Driver control endpoint (dial address).
    pub control_endpoint: String,
    #[serde(default, skip_serializing_if = "SandboxNetworkInfo::is_empty")]
    pub network: SandboxNetworkInfo,
    #[serde(default, skip_serializing_if = "ResourceSpec::is_empty")]
    pub resources_actual: ResourceSpec,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFailureSeverity {
    Unhealthy,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFailureReason {
    OomKilled,
    NodeLost,
    ImagePull,
    Admission,
    ProbeTimeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFailed {
    pub sandbox_id: SandboxId,
    pub severity: SandboxFailureSeverity,
    pub reason_code: SandboxFailureReason,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_raw: Option<serde_json::Value>,
}

/// Alias used when publishing `sandbox.unhealthy` (same payload shape).
pub type SandboxUnhealthy = SandboxFailed;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxStopping {
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDestroyed {
    pub sandbox_id: SandboxId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxArchived {
    pub sandbox_id: SandboxId,
    /// Where retained volumes/snapshots/logs can be found.
    pub archive_ref: String,
}
