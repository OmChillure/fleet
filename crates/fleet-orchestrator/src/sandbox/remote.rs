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
    /// Use case: `on_pc` | `in_container`.
    #[serde(default)]
    pub run_mode: String,
    /// `inherit` | `none` | `allowlist` | `open`. Empty means inherit.
    #[serde(default)]
    pub egress_mode: String,
    /// Extra destinations when `egress_mode` is `allowlist`.
    #[serde(default)]
    pub egress_allowlist: String,
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
    /// Relative text files the guest must see (TEAM.md, work/, handoffs).
    /// The plane disk is not shared with the runner.
    #[serde(default)]
    pub workspace_files: HashMap<String, String>,
    pub env: HashMap<String, String>,
}

fn is_vendor_guest_env(key: &str) -> bool {
    matches!(
        key,
        "XAI_API_KEY" | "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN"
    )
}

fn strip_denied_secret_env(env: &mut HashMap<String, String>) {
    const DENY: &[&str] = &[
        "GROK_SESSION",
        "FLEET_GROK_SESSION",
        "AUTH_JSON",
        "FLEET_AUTH_JSON",
        "CLAUDE_SESSION",
        "FLEET_CLAUDE_SESSION",
    ];
    for k in DENY {
        env.remove(*k);
    }
    env.retain(|k, _| {
        let u = k.to_ascii_uppercase();
        !u.contains("AUTH.JSON") && !u.contains("AUTH_JSON") && !u.ends_with("_SESSION_KEY")
    });
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
        .filter(|(k, _)| k.starts_with("FLEET_") && !is_vendor_guest_env(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.remove("FLEET_WORK_DIR");
    strip_denied_secret_env(&mut env);
    env.insert("FLEET_WORKER_TOKEN".into(), req.worker_token.clone());
    env.insert("FLEET_ORCHESTRATOR_URL".into(), orchestrator_url.into());
    let egress_mode = env.get("FLEET_EGRESS").cloned().unwrap_or_default();
    let egress_allowlist = env
        .get("FLEET_EGRESS_ALLOWLIST")
        .cloned()
        .unwrap_or_default();
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
        run_mode: match runtime {
            "container" | "docker" | "k8s" | "kubernetes" => "in_container".into(),
            _ => "on_pc".into(),
        },
        egress_mode,
        egress_allowlist,
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
        workspace_files: collect_workspace_files(&req.work_dir, req.persona_md.as_deref()),
        env,
    }
}

/// Pack the session workdir the tick just prepared so the runner can
/// recreate it. Caps live in `agent_fleet_contracts`.
pub fn collect_workspace_files(
    work_dir: &std::path::Path,
    persona_md: Option<&str>,
) -> HashMap<String, String> {
    let mut files: HashMap<String, String> = agent_fleet_contracts::collect_guest_files(work_dir)
        .into_iter()
        .collect();
    if let Some(md) = persona_md.map(str::trim).filter(|s| !s.is_empty()) {
        files
            .entry("PERSONA.md".into())
            .or_insert_with(|| md.to_string());
    }
    files
        .entry("fleet-tool.sh".into())
        .or_insert_with(|| agent_fleet_contracts::FLEET_TOOL_SH.to_string());
    files
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
    }

    #[test]
    fn host_id_from_handle_none_for_bare_uuid() {
        assert_eq!(
            host_id_from_handle("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            None
        );
    }

    #[test]
    fn host_id_from_handle_none_for_wrong_prefix() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        assert_eq!(host_id_from_handle(&format!("remote:{id}")), None);
        assert_eq!(host_id_from_handle(&format!("Host:{id}")), None);
    }

    #[test]
    fn host_id_from_handle_none_for_host_non_uuid() {
        assert_eq!(host_id_from_handle("host:"), None);
        assert_eq!(host_id_from_handle("host:not-a-uuid"), None);
    }

    #[test]
    fn spawn_payload_shape_matches_contract() {
        let req = sample_req();
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "host", 3600, host_id);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["session_id"], req.session_id.to_string());
        assert_eq!(v["project_id"], req.project_id.to_string());
        assert_eq!(v["run_id"], req.run_id.to_string());
        assert_eq!(v["board_task_id"], req.board_task_id.to_string());
        assert_eq!(v["plan_task_id"], "t1");
        assert_eq!(v["worker_profile"], "coding");
        assert_eq!(v["task_title"], "Do it");
        assert_eq!(v["task_spec"], "spec");
        assert_eq!(v["isolation"], "process");
        assert_eq!(v["run_mode"], "on_pc");
        assert_eq!(p.kind, "spawn_session");
        assert_eq!(p.worker_token, "tok-plain");
        assert!(!p.env.contains_key("FLEET_WORK_DIR"));
        assert!(
            p.workspace_files.contains_key("fleet-tool.sh"),
            "guest must ship fleet-tool.sh: {:?}",
            p.workspace_files.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn spawn_payload_container_claim_strips_host_absolute_bins() {
        let mut req = sample_req();
        req.env.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/home/viny/.local/bin/mock-acp-agent".into(),
        );
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "container", 3600, host_id);
        assert_eq!(p.isolation, "container");
        assert!(
            !p.env.contains_key("FLEET_MOCK_ACP_BIN"),
            "container claims must not carry host-absolute bins: {:?}",
            p.env
        );
    }

    #[test]
    fn spawn_payload_process_runtime_keeps_host_absolute_bins() {
        let mut req = sample_req();
        req.env.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/home/viny/.local/bin/mock-acp-agent".into(),
        );
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        for runtime in ["host", "process"] {
            let p = spawn_payload(&req, "http://172.18.0.1:8080", runtime, 3600, host_id);
            assert_eq!(p.isolation, "process");
            assert_eq!(
                p.env.get("FLEET_MOCK_ACP_BIN").map(String::as_str),
                Some("/home/viny/.local/bin/mock-acp-agent"),
                "{runtime} claims must keep host-absolute bins: {:?}",
                p.env
            );
        }
    }

    #[test]
    fn spawn_payload_drops_non_fleet_oauth_env() {
        let mut req = sample_req();
        req.env.insert("GITHUB_TOKEN".into(), "gh-secret".into());
        req.env.insert("access_token".into(), "oauth-access".into());
        req.env
            .insert("client_secret".into(), "oauth-client".into());
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "host", 3600, host_id);
        assert!(
            p.env.keys().all(|k| k.starts_with("FLEET_")),
            "payload env must be FLEET_* only: {:?}",
            p.env
        );
        assert!(!p.env.contains_key("GITHUB_TOKEN"));
        assert!(!p.env.contains_key("access_token"));
        assert!(!p.env.contains_key("client_secret"));
        assert_eq!(
            p.env.get("FLEET_SESSION_ID").map(String::as_str),
            Some("11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn spawn_payload_copies_repo_url_and_git_ref_from_fleet_env() {
        let mut req = sample_req();
        req.env.insert(
            "FLEET_REPO_URL".into(),
            "https://git.example/repo.git".into(),
        );
        req.env.insert("FLEET_GIT_REF".into(), "main".into());
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "host", 3600, host_id);
        assert_eq!(p.repo_url.as_deref(), Some("https://git.example/repo.git"));
        assert_eq!(p.git_ref.as_deref(), Some("main"));

        req.env.insert("FLEET_REPO_URL".into(), "  \n".into());
        req.env.insert("FLEET_GIT_REF".into(), "   ".into());
        let p = spawn_payload(&req, "http://172.18.0.1:8080", "host", 3600, host_id);
        assert_eq!(p.repo_url, None);
        assert_eq!(p.git_ref, None);
    }

    #[test]
    fn workspace_files_include_team_and_work() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("TEAM.md"), "you are t1\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work/app.rs"), "fn main() {}\n").unwrap();
        let files = collect_workspace_files(dir.path(), Some("# persona\n"));
        assert_eq!(files.get("TEAM.md").unwrap(), "you are t1\n");
        assert_eq!(files.get("work/app.rs").unwrap(), "fn main() {}\n");
        assert!(files.get("PERSONA.md").unwrap().contains("persona"));
        assert!(files.get("fleet-tool.sh").unwrap().contains("fleet-tool"));
    }

    #[test]
    fn payload_carries_official_cli_harness_and_persona() {
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        for (backend, want) in [
            (Some("grok"), "grok"),
            (Some("xai"), "grok"),
            (Some("claude"), "claude"),
            (Some("ollama"), "ollama"),
            (None, "mock"),
        ] {
            let mut req = sample_req();
            req.agent_backend = backend.map(str::to_string);
            req.persona_md = Some("# Persona: reviewer\n".into());
            req.env.insert("FLEET_HARNESS".into(), want.to_string());
            let p = spawn_payload(&req, "http://api.example:8080", "host", 60, host_id);
            assert_eq!(
                p.env.get("FLEET_HARNESS").map(String::as_str),
                Some(want),
                "backend={backend:?} payload={p:?}"
            );
            assert_eq!(p.persona_md.as_deref(), Some("# Persona: reviewer\n"));
        }
    }
}
