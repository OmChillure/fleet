//! Driver / workspace domain commands and events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{SandboxId, SessionId, ToolCallId, TurnId};
use crate::types::{
    AgentTurnMode, PermissionDecision, RepoAuthVia, ShutdownMode,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSpec {
    pub url: String,
    /// Git ref (branch, tag, or commit). Serialized as `ref`.
    #[serde(rename = "ref")]
    pub ref_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    pub auth_via: RepoAuthVia,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInstallSpec {
    pub package: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMount {
    pub name: String,
    pub uri: String,
    pub mount_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionWorkspace {
    pub session_id: SessionId,
    pub sandbox_id: SandboxId,
    pub repo: RepoSpec,
    pub agent: AgentInstallSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<CacheMount>,
    /// Non-secret environment only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_public: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartAgentTurn {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_parts: Option<serde_json::Value>,
    pub mode: AgentTurnMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAgentTurn {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverPermissionDecision {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub decision: PermissionDecision,
    /// If the Orchestrator already executed a proxied tool, sanitized result goes here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_override: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectHumanMessage {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub text: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownDriver {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub reason: String,
    pub mode: ShutdownMode,
}

// --- events ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverHeartbeat {
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProvisioning {
    pub session_id: SessionId,
    pub sandbox_id: SandboxId,
    /// e.g. `clone_started`, `agent_installed`
    pub milestone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReady {
    pub session_id: SessionId,
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFailed {
    pub session_id: SessionId,
    pub sandbox_id: SandboxId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverShutdownCompleted {
    pub sandbox_id: SandboxId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub reason: String,
}
