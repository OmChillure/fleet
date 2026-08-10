//! Sandbox runtime: `stub` | `process` | `docker` (`SANDBOX_BACKEND`).
//! Only `FLEET_*` env + task files enter sandboxes — no OAuth tokens.
//!
//! `process` is main's fleet-worker. `local`/`shell` is Om's `bash fleet_worker.sh`.

pub mod agent_loop;
mod docker;
pub(crate) mod git_isolate;
pub mod team;
mod worker_script;

pub use docker::{docker_runtime, DockerRuntime};
pub use worker_script::{extract_bash_blocks, materialize_workspace};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Request to start a worker sandbox for a board task attempt.
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
    /// Task title for work_dir/task.md (docker/process materialize; no secrets).
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
}

/// Opaque handle returned by a successful spawn (pid, container id, stub key, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeHandle {
    pub backend: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxState {
    Running,
    Exited { code: Option<i32> },
    Unknown,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("signal failed: {0}")]
    Signal(String),
    #[error("destroy failed: {0}")]
    Destroy(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Lifecycle API owned by the orchestrator tick (design §Worker / Sandbox Adapter).
#[async_trait]
pub trait SandboxRuntime: Send + Sync {
    fn backend_name(&self) -> &'static str;

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError>;

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError>;

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError>;

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError>;

    /// List live handles for orphan reaping (process backend uses pidfiles; stub uses in-memory).
    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError>;
}

/// FLEET_* env keys injected for workers (design normative contract).
pub fn fleet_worker_env(req: &SpawnRequest, orchestrator_url: &str) -> HashMap<String, String> {
    let mut env = req.env.clone();
    env.insert("FLEET_SESSION_ID".into(), req.session_id.to_string());
    env.insert("FLEET_PROJECT_ID".into(), req.project_id.to_string());
    env.insert("FLEET_RUN_ID".into(), req.run_id.to_string());
    env.insert("FLEET_BOARD_TASK_ID".into(), req.board_task_id.to_string());
    // Design alias
    env.insert("FLEET_TASK_ID".into(), req.board_task_id.to_string());
    env.insert("FLEET_PLAN_TASK_ID".into(), req.plan_task_id.clone());
    env.insert("FLEET_WORKER_TOKEN".into(), req.worker_token.clone());
    env.insert("FLEET_WORKER_PROFILE".into(), req.worker_profile.clone());
    env.insert("FLEET_ORCHESTRATOR_URL".into(), orchestrator_url.into());
    env.insert("FLEET_API_BASE".into(), orchestrator_url.into());
    env.insert(
        "FLEET_WORK_DIR".into(),
        req.work_dir.display().to_string(),
    );
    env
}

// --- Stub backend ---

#[derive(Debug, Clone)]
struct StubEntry {
    project_id: Uuid,
    handle: RuntimeHandle,
    started_at: DateTime<Utc>,
    duration: Duration,
    destroyed: bool,
    signaled_term: bool,
    signaled_kill: bool,
}

/// In-process fake sandbox: "runs" for `stub_duration` then appears exited.
pub struct StubRuntime {
    duration: Duration,
    inner: Mutex<HashMap<String, StubEntry>>,
}

impl StubRuntime {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_default_duration() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

#[async_trait]
impl SandboxRuntime for StubRuntime {
    fn backend_name(&self) -> &'static str {
        "stub"
    }

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError> {
        let handle = RuntimeHandle {
            backend: "stub".into(),
            id: req.session_id.to_string(),
        };
        let mut g = self.inner.lock().await;
        g.insert(
            handle.id.clone(),
            StubEntry {
                project_id: req.project_id,
                handle: handle.clone(),
                started_at: Utc::now(),
                duration: self.duration,
                destroyed: false,
                signaled_term: false,
                signaled_kill: false,
            },
        );
        Ok(handle)
    }

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError> {
        let mut g = self.inner.lock().await;
        let e = g
            .get_mut(&handle.id)
            .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
        match signal {
            Signal::Term => e.signaled_term = true,
            Signal::Kill => {
                e.signaled_kill = true;
                e.destroyed = true;
            }
        }
        Ok(())
    }

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError> {
        let mut g = self.inner.lock().await;
        if let Some(e) = g.get_mut(&handle.id) {
            e.destroyed = true;
            Ok(())
        } else {
            // Idempotent destroy
            Ok(())
        }
    }

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError> {
        let g = self.inner.lock().await;
        let e = g
            .get(&handle.id)
            .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
        if e.destroyed || e.signaled_kill {
            return Ok(SandboxState::Exited { code: Some(0) });
        }
        if e.signaled_term {
            return Ok(SandboxState::Exited { code: Some(143) });
        }
        let elapsed = Utc::now().signed_duration_since(e.started_at);
        if elapsed.to_std().unwrap_or_default() >= e.duration {
            Ok(SandboxState::Exited { code: Some(0) })
        } else {
            Ok(SandboxState::Running)
        }
    }

    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError> {
        let g = self.inner.lock().await;
        Ok(g.values()
            .filter(|e| e.project_id == project_id && !e.destroyed)
            .map(|e| e.handle.clone())
            .collect())
    }
}

mod process;
mod process_om;

pub use process::{build_process_runtime, worker_argv_from_env, ProcessRuntime};
pub use process_om::process_runtime;

/// Options for [`build_runtime`].
#[derive(Debug, Clone)]
pub struct RuntimeBuildOpts {
    pub backend: String,
    pub stub_duration: Duration,
    pub data_dir: PathBuf,
    /// argv for process backend (program + args). Empty → env `PROCESS_WORKER_BIN` / default.
    pub worker_argv: Vec<String>,
}

/// Build runtime from config backend name.
pub fn build_runtime(opts: &RuntimeBuildOpts) -> Result<Arc<dyn SandboxRuntime>, SandboxError> {
    match opts.backend.as_str() {
        "stub" => Ok(Arc::new(StubRuntime::new(opts.stub_duration))),
        "process" => {
            let argv = if opts.worker_argv.is_empty() {
                worker_argv_from_env()
            } else {
                opts.worker_argv.clone()
            };
            Ok(Arc::new(ProcessRuntime::new(&opts.data_dir, argv)?))
        }
        "local" | "shell" => Ok(process_runtime()),
        "docker" | "container" => Ok(docker_runtime()),
        other => Err(SandboxError::Spawn(format!(
            "unknown sandbox backend {other:?} (supports stub|process|local|docker)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        }
    }

    #[tokio::test]
    async fn stub_spawn_running_then_exit() {
        let rt = StubRuntime::new(Duration::from_millis(50));
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let h = rt.spawn(sample_req(session, project)).await.unwrap();
        assert_eq!(h.backend, "stub");
        assert_eq!(h.id, session.to_string());
        assert_eq!(rt.state(&h).await.unwrap(), SandboxState::Running);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            rt.state(&h).await.unwrap(),
            SandboxState::Exited { code: Some(0) }
        );
    }

    #[tokio::test]
    async fn stub_destroy_idempotent() {
        let rt = StubRuntime::new(Duration::from_secs(60));
        let project = Uuid::new_v4();
        let h = rt.spawn(sample_req(Uuid::new_v4(), project)).await.unwrap();
        rt.destroy(&h).await.unwrap();
        rt.destroy(&h).await.unwrap();
        assert_eq!(
            rt.state(&h).await.unwrap(),
            SandboxState::Exited { code: Some(0) }
        );
    }

    #[tokio::test]
    async fn stub_signal_term_and_kill() {
        let rt = StubRuntime::new(Duration::from_secs(60));
        let project = Uuid::new_v4();
        let h = rt.spawn(sample_req(Uuid::new_v4(), project)).await.unwrap();
        rt.signal(&h, Signal::Term).await.unwrap();
        assert_eq!(
            rt.state(&h).await.unwrap(),
            SandboxState::Exited { code: Some(143) }
        );
        let h2 = rt.spawn(sample_req(Uuid::new_v4(), project)).await.unwrap();
        rt.signal(&h2, Signal::Kill).await.unwrap();
        assert_eq!(
            rt.state(&h2).await.unwrap(),
            SandboxState::Exited { code: Some(0) }
        );
    }

    #[tokio::test]
    async fn stub_list_filters_project_and_destroyed() {
        let rt = StubRuntime::new(Duration::from_secs(60));
        let p1 = Uuid::new_v4();
        let p2 = Uuid::new_v4();
        let h1 = rt.spawn(sample_req(Uuid::new_v4(), p1)).await.unwrap();
        let _h2 = rt.spawn(sample_req(Uuid::new_v4(), p1)).await.unwrap();
        let _h3 = rt.spawn(sample_req(Uuid::new_v4(), p2)).await.unwrap();
        rt.destroy(&h1).await.unwrap();
        let list = rt.list(p1).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(rt.list(p2).await.unwrap().len(), 1);
    }

    #[test]
    fn fleet_env_keys() {
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        let req = sample_req(session, project);
        let env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        assert_eq!(env.get("FLEET_SESSION_ID").unwrap(), &session.to_string());
        assert_eq!(env.get("FLEET_PROJECT_ID").unwrap(), &project.to_string());
        assert_eq!(env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(
            env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn build_runtime_stub() {
        let opts = RuntimeBuildOpts {
            backend: "stub".into(),
            stub_duration: Duration::from_secs(1),
            data_dir: PathBuf::from("/tmp"),
            worker_argv: vec![],
        };
        let rt = build_runtime(&opts).unwrap();
        assert_eq!(rt.backend_name(), "stub");
        let mut docker = opts.clone();
        docker.backend = "docker".into();
        assert_eq!(build_runtime(&docker).unwrap().backend_name(), "docker");
        let mut local = opts.clone();
        local.backend = "local".into();
        assert_eq!(build_runtime(&local).unwrap().backend_name(), "process");
        let mut bad = opts.clone();
        bad.backend = "k8s".into();
        assert!(build_runtime(&bad).is_err());
    }
}
