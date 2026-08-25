//! Shared enums and small value objects used by multiple domains.

use serde::{Deserialize, Serialize};

/// Durable session lifecycle states (Orchestrator-owned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Pending,
    AwaitingSandbox,
    Provisioning,
    Ready,
    Running,
    WaitingHitl,
    Stopping,
    Completed,
    Failed,
    Cancelled,
    Archived,
}

/// Permission outcome for a proposed tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    AllowOnce,
    AlwaysAllowForSession,
}

/// Agent turn execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTurnMode {
    Autonomous,
    Interactive,
}

/// How secrets reach git / registries — never raw secrets in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoAuthVia {
    Proxy,
    DeployKeyRef,
}

/// Side-effect class for tool-call policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    Read,
    Write,
    Network,
    Destructive,
}

/// Trace redaction level for stored / forwarded agent events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Redaction {
    None,
    Partial,
    Full,
}

/// Driver shutdown mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    Graceful,
    Immediate,
}

/// How compute is *placed* (k8s cluster, a VM host, podman, …).
///
/// This is **not** isolation. Isolation is [`crate::IsolationClass`]
/// (`process` < `container` < `vm`). A k8s pod is still a container.
///
/// Serializes as a plain JSON string (`"k8s"`, `"vm"`, …). Unknown values
/// round-trip as [`SandboxBackend::Other`] without rewriting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SandboxBackend {
    K8s,
    Vm,
    Podman,
    SelfHosted,
    Other(String),
}

impl Serialize for SandboxBackend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::K8s => "k8s",
            Self::Vm => "vm",
            Self::Podman => "podman",
            Self::SelfHosted => "self_hosted",
            Self::Other(other) => return serializer.serialize_str(other),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for SandboxBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "k8s" => Self::K8s,
            "vm" => Self::Vm,
            "podman" => Self::Podman,
            "self_hosted" => Self::SelfHosted,
            other => Self::Other(other.to_string()),
        })
    }
}

/// Network egress posture for a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    None,
    Allowlist,
    PackageRegistries,
    /// Discouraged — product vision prefers tight egress.
    Open,
}

/// Artifact kinds published back to integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    PullRequest,
    File,
    Url,
    Screenshot,
    Log,
}

/// Review feedback kinds that can wake a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFeedbackKind {
    CiFailure,
    ReviewComment,
    MergeConflict,
}

/// Risk level for HITL permission prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Reference to bulky data stored outside the bus message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: ArtifactKind,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Resource requests / actuals for a sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
}

impl ResourceSpec {
    /// True when no resource fields are set.
    pub fn is_empty(&self) -> bool {
        self.cpu.is_none() && self.memory.is_none() && self.disk.is_none() && self.gpu.is_none()
    }
}

/// Network policy applied at sandbox create time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub egress_mode: EgressMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<String>,
}

/// Optional network identity reported on `sandbox.ready`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxNetworkInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

impl SandboxNetworkInfo {
    /// True when no network identity fields are set.
    pub fn is_empty(&self) -> bool {
        self.ip.is_none() && self.hostname.is_none()
    }
}
