//! FleetSandbox CRD — Kubernetes view of a worker sandbox.
//!
//! Aligns with `docs/architecture/events.md` `sandbox.create` and
//! `agent-fleet-contracts::sandbox::CreateSandbox`.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CRD_GROUP: &str = "fleet.agentfleet.io";
pub const CRD_VERSION: &str = "v1alpha1";
pub const CRD_KIND: &str = "FleetSandbox";
pub const LABEL_SANDBOX: &str = "fleet.agentfleet.io/sandbox";
pub const LABEL_SESSION: &str = "fleet.agentfleet.io/session-id";
pub const LABEL_WORKSPACE: &str = "fleet.agentfleet.io/workspace-id";
pub const LABEL_PROJECT: &str = "fleet.agentfleet.io/project-id";
pub const FINALIZER: &str = "fleet.agentfleet.io/sandbox-cleanup";

/// Namespaced worker sandbox. Operator turns this into a Pod + NetworkPolicy.
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "fleet.agentfleet.io",
    version = "v1alpha1",
    kind = "FleetSandbox",
    plural = "fleetsandboxes",
    shortname = "fsb",
    namespaced,
    status = "FleetSandboxStatus",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Pod","type":"string","jsonPath":".status.podName"}"#,
    printcolumn = r#"{"name":"Session","type":"string","jsonPath":".spec.sessionId"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct FleetSandboxSpec {
    /// Workspace / org scope (orchestrator project_id is often the same).
    pub workspace_id: String,
    pub session_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_profile: Option<String>,
    /// Container image (typically `agent-fleet-api` which includes `fleet-worker`).
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Extra env (FLEET_* worker contract). Secrets should be refs, not raw values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<SandboxResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<SandboxNetwork>,
    /// Hard lifetime from create. Operator deletes the CR when exceeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<i64>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_storage: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxNetwork {
    /// `none` (default): DNS + control-plane API only.
    /// `dns_only`: same as none.
    /// `allowlist`: plus `allowlist` CIDRs.
    #[serde(default = "default_egress")]
    pub egress_mode: String,
    /// CIDRs, e.g. `10.0.0.0/8`. Hostnames are not enforceable via NetworkPolicy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<String>,
}

fn default_egress() -> String {
    "none".into()
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FleetSandboxStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

impl FleetSandboxSpec {
    pub fn egress_mode(&self) -> &str {
        self.network
            .as_ref()
            .map(|n| n.egress_mode.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("none")
    }
}

/// DNS-1123 label from a CR / session name.
pub fn dns_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let mut s = out.trim_matches('-').to_string();
    if s.is_empty() {
        s = "sandbox".into();
    }
    s.truncate(63);
    s.trim_end_matches('-').to_string()
}

pub fn pod_name_for(sandbox_name: &str) -> String {
    let mut n = format!("fsb-{}", dns_label(sandbox_name));
    n.truncate(63);
    n.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    #[test]
    fn dns_label_sanitizes() {
        assert_eq!(dns_label("Demo_T1"), "demo-t1");
        assert_eq!(dns_label("!!!"), "sandbox");
        assert!(dns_label(&"x".repeat(80)).len() <= 63);
    }

    #[test]
    fn crd_identity() {
        let crd = FleetSandbox::crd();
        assert_eq!(crd.spec.group, CRD_GROUP);
        assert_eq!(crd.spec.names.kind, CRD_KIND);
        assert_eq!(crd.spec.names.plural, "fleetsandboxes");
    }
}
