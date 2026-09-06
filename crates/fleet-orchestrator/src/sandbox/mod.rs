//! Workspace prep + remote assign payload for paired `fleet-host`.
//! The control plane does not run sandboxes. Isolation is on the host.
//! Only `FLEET_*` env + task files leave the plane — no OAuth tokens.

pub mod agent_loop;
pub(crate) mod git_isolate;
pub mod io;
pub mod remote;
pub mod share;
pub mod team;
pub mod team_chat;
mod worker_script;

pub use worker_script::{extract_bash_blocks, materialize_workspace};

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// Workspace-prep failure (files written before a host claim is enqueued).
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Request to start a worker on a paired host for a board task attempt.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub board_task_id: Uuid,
    pub plan_task_id: String,
    pub worker_profile: String,
    pub worker_token: String,
    pub work_dir: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    /// Task title for work_dir/task.md (no secrets).
    pub task_title: Option<String>,
    pub task_summary: Option<String>,
    /// Full task spec markdown; ```bash fences become commands.sh.
    pub task_spec: Option<String>,
    /// Agency-agents persona name (display).
    pub persona_name: Option<String>,
    /// Full persona markdown written to PERSONA.md.
    pub persona_md: Option<String>,
    /// Resolved worker LLM: `grok` (env) or `ollama` (settings). None = bash worker.
    pub agent_backend: Option<String>,
    /// Model id for the worker LLM (e.g. grok-4.5, llama3.2).
    pub agent_model: Option<String>,
    /// Control-plane integration proxy (tokens never enter the sandbox).
    pub io: io::IntegrationHandle,
}

/// FLEET_* env keys injected for workers (design normative contract).
pub fn fleet_worker_env(req: &SpawnRequest, orchestrator_url: &str) -> HashMap<String, String> {
    let mut env = req.env.clone();
    env.insert("FLEET_SESSION_ID".into(), req.session_id.to_string());
    env.insert("FLEET_PROJECT_ID".into(), req.project_id.to_string());
    env.insert("FLEET_RUN_ID".into(), req.run_id.to_string());
    env.insert("FLEET_BOARD_TASK_ID".into(), req.board_task_id.to_string());
    env.insert("FLEET_TASK_ID".into(), req.board_task_id.to_string());
    env.insert("FLEET_PLAN_TASK_ID".into(), req.plan_task_id.clone());
    env.insert("FLEET_WORKER_TOKEN".into(), req.worker_token.clone());
    env.insert("FLEET_WORKER_PROFILE".into(), req.worker_profile.clone());
    env.insert("FLEET_ORCHESTRATOR_URL".into(), orchestrator_url.into());
    env.insert("FLEET_API_BASE".into(), orchestrator_url.into());
    env.insert("FLEET_WORK_DIR".into(), req.work_dir.display().to_string());
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_req(session: Uuid, project: Uuid) -> SpawnRequest {
        SpawnRequest {
            session_id: session,
            project_id: project,
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: PathBuf::from("/tmp/fleet-test"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            task_title: None,
            task_summary: None,
            task_spec: None,
            persona_name: None,
            persona_md: None,
            agent_backend: None,
            agent_model: None,
            io: io::IntegrationHandle::none(),
        }
    }

    #[test]
    fn fleet_worker_env_injects_normative_fleet_keys_and_aliases() {
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        let req = sample_req(session, project);
        let env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        assert_eq!(env.get("FLEET_SESSION_ID").unwrap(), &session.to_string());
        assert_eq!(env.get("FLEET_PROJECT_ID").unwrap(), &project.to_string());
        assert_eq!(env.get("FLEET_RUN_ID").unwrap(), &req.run_id.to_string());
        assert_eq!(
            env.get("FLEET_BOARD_TASK_ID").unwrap(),
            &req.board_task_id.to_string()
        );
        assert_eq!(
            env.get("FLEET_TASK_ID").unwrap(),
            &req.board_task_id.to_string()
        );
        assert_ne!(env.get("FLEET_TASK_ID").unwrap(), "t1");
        assert_eq!(env.get("FLEET_PLAN_TASK_ID").unwrap(), "t1");
        assert_eq!(env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(env.get("FLEET_WORKER_PROFILE").unwrap(), "coding");
        assert_eq!(
            env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            env.get("FLEET_API_BASE").unwrap(),
            env.get("FLEET_ORCHESTRATOR_URL").unwrap()
        );
        assert_eq!(env.get("FLEET_WORK_DIR").unwrap(), "/tmp/fleet-test");
    }

    #[test]
    fn fleet_worker_env_sets_run_board_plan_profile_and_work_dir() {
        let req = sample_req(Uuid::new_v4(), Uuid::new_v4());
        let env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        assert_eq!(env.get("FLEET_RUN_ID").unwrap(), &req.run_id.to_string());
        assert_eq!(
            env.get("FLEET_BOARD_TASK_ID").unwrap(),
            &req.board_task_id.to_string()
        );
        assert_eq!(env.get("FLEET_PLAN_TASK_ID").unwrap(), "t1");
        assert_eq!(env.get("FLEET_WORKER_PROFILE").unwrap(), "coding");
        assert_eq!(env.get("FLEET_WORK_DIR").unwrap(), "/tmp/fleet-test");
    }

    #[test]
    fn fleet_worker_env_task_id_aliases_board_not_plan() {
        let req = sample_req(Uuid::new_v4(), Uuid::new_v4());
        let env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        assert_eq!(
            env.get("FLEET_TASK_ID").unwrap(),
            &req.board_task_id.to_string()
        );
        assert_ne!(env.get("FLEET_TASK_ID").unwrap(), "t1");
    }

    #[test]
    fn fleet_worker_env_api_base_aliases_orchestrator_url() {
        let req = sample_req(Uuid::new_v4(), Uuid::new_v4());
        let env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        assert_eq!(env.get("FLEET_API_BASE").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(
            env.get("FLEET_API_BASE").unwrap(),
            env.get("FLEET_ORCHESTRATOR_URL").unwrap()
        );
    }
}
