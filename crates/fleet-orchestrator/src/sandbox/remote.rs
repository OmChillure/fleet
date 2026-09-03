//! Remote worker-host handle + `work_claims` spawn payload.
//!
//! Production assignment is `runtime_backend = "remote"` and
//! `runtime_handle = "host:{id}"`. The plane does not run a local runtime.

use super::SpawnRequest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub const BACKEND: &str = "remote";

/// Session `runtime_handle` for a job leased to `host_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteHostHandle {
    pub host_id: Uuid,
}

impl RemoteHostHandle {
    pub fn new(host_id: Uuid) -> Self {
        Self { host_id }
    }

    pub fn as_str(&self) -> String {
        runtime_handle(self.host_id)
    }
}

pub fn runtime_handle(host_id: Uuid) -> String {
    format!("host:{host_id}")
}

pub fn host_id_from_handle(handle: &str) -> Option<Uuid> {
    handle.strip_prefix("host:")?.parse().ok()
}

/// `work_claims.payload` for `kind = spawn_session`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpawnPayload {
    pub kind: String,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub board_task_id: Uuid,
    pub plan_task_id: String,
    pub worker_profile: String,
    pub worker_token: String,
    pub orchestrator_url: String,
    pub runtime: String,
    /// Isolation class: process | container | vm. Not "k8s".
    #[serde(default)]
    pub isolation: String,
    pub task_title: String,
    pub task_spec: String,
    pub timeout_s: i32,
    pub host_id: Uuid,
    pub repo_url: Option<String>,
    pub git_ref: Option<String>,
    #[serde(default)]
    pub persona_md: Option<String>,
    pub env: HashMap<String, String>,
}

fn isolation_for_runtime(runtime: &str) -> &'static str {
    match runtime {
        "container" | "docker" | "k8s" | "kubernetes" => "container",
        "vm" | "firecracker" | "qemu" => "vm",
        _ => "process",
    }
}

pub fn spawn_payload(
    req: &SpawnRequest,
    orchestrator_url: &str,
    runtime: &str,
    timeout_s: i32,
    host_id: Uuid,
) -> SpawnPayload {
    // Tokens stay on the CP. Payload env is FLEET_* only — no OAuth /
    // integration ciphertext (GITHUB_TOKEN, access_token, client_secret).
    let mut env: HashMap<String, String> = req
        .env
        .iter()
        .filter(|(k, _)| k.starts_with("FLEET_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.remove("FLEET_WORK_DIR");
    env.insert("FLEET_WORKER_TOKEN".into(), req.worker_token.clone());
    env.insert("FLEET_ORCHESTRATOR_URL".into(), orchestrator_url.into());
    let isolation = isolation_for_runtime(runtime);
    if isolation == "container" || isolation == "vm" {
        agent_fleet_contracts::strip_isolated_guest_env(&mut env);
    }
    SpawnPayload {
        kind: "spawn_session".into(),
        session_id: req.session_id,
        project_id: req.project_id,
        run_id: req.run_id,
        board_task_id: req.board_task_id,
        plan_task_id: req.plan_task_id.clone(),
        worker_profile: req.worker_profile.clone(),
        worker_token: req.worker_token.clone(),
        orchestrator_url: orchestrator_url.into(),
        runtime: runtime.into(),
        isolation: isolation.into(),
        task_title: req.task_title.clone().unwrap_or_default(),
        task_spec: req.task_spec.clone().unwrap_or_default(),
        timeout_s,
        host_id,
        repo_url: req
            .env
            .get("FLEET_REPO_URL")
            .cloned()
            .filter(|s| !s.trim().is_empty()),
        git_ref: req
            .env
            .get("FLEET_GIT_REF")
            .cloned()
            .filter(|s| !s.trim().is_empty()),
        persona_md: req.persona_md.clone().filter(|s| !s.trim().is_empty()),
        env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn sample_req() -> SpawnRequest {
        let session = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        SpawnRequest {
            session_id: session,
            project_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            run_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            board_task_id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok-plain".into(),
            work_dir: PathBuf::from("/tmp/fleet-test"),
            env: HashMap::from([
                ("FLEET_SESSION_ID".into(), session.to_string()),
                ("FLEET_WORKER_TOKEN".into(), "tok-plain".into()),
                ("FLEET_WORK_DIR".into(), "/tmp/fleet-test".into()),
            ]),
            timeout: Duration::from_secs(120),
            task_title: Some("Do it".into()),
            task_summary: None,
            task_spec: Some("spec".into()),
            persona_name: None,
            persona_md: None,
            agent_backend: None,
            agent_model: None,
            io: crate::sandbox::io::IntegrationHandle::none(),
        }
    }

    #[test]
    fn handle_roundtrip() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let h = runtime_handle(id);
        assert_eq!(h, "host:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(host_id_from_handle(&h), Some(id));
        assert_eq!(host_id_from_handle("stub-1"), None);
        assert_eq!(RemoteHostHandle::new(id).as_str(), h);
        assert_eq!(BACKEND, "remote");
    }

    #[test]
    fn payload_includes_plaintext_token_and_env() {
        let mut req = sample_req();
        req.env.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/home/viny/.local/bin/mock-acp-agent".into(),
        );
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "container", 3600, host_id);
        assert!(
            !p.env.contains_key("FLEET_MOCK_ACP_BIN"),
            "container claims must not carry host-absolute bins: {:?}",
            p.env
        );
        assert_eq!(p.kind, "spawn_session");
        assert_eq!(p.worker_token, "tok-plain");
        assert_eq!(p.runtime, "container");
        assert_eq!(p.isolation, "container");
        assert_eq!(p.orchestrator_url, "http://172.18.0.1:8080");
        assert_eq!(p.timeout_s, 3600);
        assert_eq!(p.host_id, host_id);
        assert_eq!(p.env.get("FLEET_WORKER_TOKEN").unwrap(), "tok-plain");
        assert_eq!(
            p.env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            "http://172.18.0.1:8080"
        );
        assert!(!p.env.contains_key("FLEET_WORK_DIR"));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["kind"], "spawn_session");
        assert_eq!(v["worker_token"], "tok-plain");
        assert_eq!(v["runtime"], "container");
        assert_eq!(v["host_id"], host_id.to_string());
        assert!(v["repo_url"].is_null());
        assert!(v["git_ref"].is_null());
        assert!(v["env"].get("FLEET_WORK_DIR").is_none());
    }
}
