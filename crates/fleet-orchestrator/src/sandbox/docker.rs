//! Docker sandbox: one container per session, workdir bind-mounted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::worker_script::materialize_workspace;
use super::{
    RuntimeHandle, SandboxError, SandboxRuntime, SandboxState, Signal, SpawnRequest,
};

struct DockerEntry {
    project_id: Uuid,
    handle: RuntimeHandle,
    container_id: String,
    work_dir: PathBuf,
    agent: Option<super::agent_loop::AgentJob>,
    last_exit: Option<i32>,
    destroyed: bool,
}

pub struct DockerRuntime {
    image: String,
    network: String,
    inner: Mutex<HashMap<String, DockerEntry>>,
}

impl DockerRuntime {
    pub fn new(image: impl Into<String>, network: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            network: network.into(),
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let image = std::env::var("SANDBOX_DOCKER_IMAGE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "alpine:3.20".into());
        let network = std::env::var("SANDBOX_DOCKER_NETWORK")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "bridge".into());
        Self::new(image, network)
    }
}

#[async_trait]
impl SandboxRuntime for DockerRuntime {
    fn backend_name(&self) -> &'static str {
        "docker"
    }

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError> {
        // Fail fast if docker is missing.
        ensure_docker().await?;

        materialize_workspace(&req).await?;

        let work = req.work_dir.canonicalize().unwrap_or(req.work_dir.clone());
        let name = format!("fleet-sbx-{}", req.session_id);

        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.clone(),
            "-w".into(),
            "/work".into(),
            "-v".into(),
            format!("{}:/work", work.display()),
        ];

        args.push("--network".into());
        args.push(self.network.clone());
        args.push("--pids-limit".into());
        args.push("256".into());

        for (k, v) in &req.env {
            if k.starts_with("FLEET_") {
                args.push("-e".into());
                args.push(format!("{k}={v}"));
            }
        }
        args.push("-e".into());
        args.push("FLEET_WORK_DIR=/work".into());
        args.push(self.image.clone());
        args.push("sh".into());
        args.push("-c".into());
        args.push(
            "apk add --no-cache bash >/dev/null 2>&1 || true; \
             if command -v bash >/dev/null 2>&1; then bash /work/fleet_worker.sh; \
             else sh /work/fleet_worker.sh; fi"
                .into(),
        );

        let output = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SandboxError::Spawn(format!("docker run failed to start: {e}")))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(SandboxError::Spawn(format!(
                "docker run failed: {}",
                err.trim()
            )));
        }

        let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if container_id.is_empty() {
            return Err(SandboxError::Spawn("docker run returned empty id".into()));
        }

        let handle = RuntimeHandle {
            backend: "docker".into(),
            id: req.session_id.to_string(),
        };

        let mut g = self.inner.lock().await;
        g.insert(
            handle.id.clone(),
            DockerEntry {
                project_id: req.project_id,
                handle: handle.clone(),
                container_id: container_id.clone(),
                work_dir: work.clone(),
                agent: None,
                last_exit: None,
                destroyed: false,
            },
        );

        tracing::info!(
            session_id = %req.session_id,
            container = %container_id,
            image = %self.image,
            network = %self.network,
            "docker sandbox spawned"
        );
        Ok(handle)
    }

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError> {
        let (container_id, abort_agent) = {
            let g = self.inner.lock().await;
            let e = g
                .get(&handle.id)
                .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
            if e.destroyed {
                return Ok(());
            }
            (e.container_id.clone(), e.agent.is_some())
        };
        if abort_agent {
            let mut g = self.inner.lock().await;
            if let Some(e) = g.get_mut(&handle.id)
                && let Some(agent) = e.agent.as_ref()
            {
                agent.abort();
            }
        }
        if !container_id.is_empty() {
            let sig = match signal {
                Signal::Term => "SIGTERM",
                Signal::Kill => "SIGKILL",
            };
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                Command::new("docker")
                    .args(["kill", "--signal", sig, &container_id])
                    .output(),
            )
            .await;
        }
        Ok(())
    }

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError> {
        let mut g = self.inner.lock().await;
        let Some(e) = g.get_mut(&handle.id) else {
            return Ok(());
        };
        if e.destroyed {
            return Ok(());
        }
        if let Some(agent) = e.agent.take() {
            agent.abort();
        }
        if e.last_exit.is_none() {
            e.last_exit = read_fleet_exit(&e.work_dir);
        }
        if !e.container_id.is_empty() {
            let _ = Command::new("docker")
                .args(["rm", "-f", &e.container_id])
                .output()
                .await;
        }
        e.destroyed = true;
        tracing::debug!(id = %handle.id, container = %e.container_id, "docker sandbox destroyed");
        Ok(())
    }

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError> {
        let (container_id, work_dir, destroyed, last_exit) = {
            let mut g = self.inner.lock().await;
            let e = g
                .get_mut(&handle.id)
                .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
            if let Some(agent) = e.agent.as_ref() {
                if let Some(code) = agent.poll_exit() {
                    e.last_exit = Some(code);
                    e.agent = None;
                    return Ok(SandboxState::Exited { code: Some(code) });
                }
                return Ok(SandboxState::Running);
            }
            (
                e.container_id.clone(),
                e.work_dir.clone(),
                e.destroyed,
                e.last_exit,
            )
        };

        let output = Command::new("docker")
            .args([
                "inspect",
                "-f",
                "{{.State.Running}} {{.State.ExitCode}}",
                &container_id,
            ])
            .output()
            .await;

        let resolved = match output {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut parts = text.split_whitespace();
                let running = parts.next().unwrap_or("false");
                let code: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                if running.eq_ignore_ascii_case("true") {
                    SandboxState::Running
                } else {
                    SandboxState::Exited { code: Some(code) }
                }
            }
            _ => {
                if let Some(code) = read_fleet_exit(&work_dir).or(last_exit) {
                    SandboxState::Exited { code: Some(code) }
                } else if destroyed {
                    SandboxState::Exited { code: Some(1) }
                } else {
                    SandboxState::Unknown
                }
            }
        };

        if let SandboxState::Exited { code } = resolved {
            let mut g = self.inner.lock().await;
            if let Some(e) = g.get_mut(&handle.id) {
                e.last_exit = code.or(e.last_exit);
            }
        }
        Ok(resolved)
    }

    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError> {
        let g = self.inner.lock().await;
        Ok(g.values()
            .filter(|e| e.project_id == project_id && !e.destroyed)
            .map(|e| e.handle.clone())
            .collect())
    }
}

fn read_fleet_exit(work_dir: &std::path::Path) -> Option<i32> {
    let raw = std::fs::read_to_string(work_dir.join(".fleet_exit")).ok()?;
    raw.trim().parse().ok()
}

async fn ensure_docker() -> Result<(), SandboxError> {
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("docker").args(["version", "--format", "{{.Server.Version}}"]).output(),
    )
    .await
    .map_err(|_| SandboxError::Spawn("docker version timed out".into()))?
    .map_err(|e| SandboxError::Spawn(format!("docker not available: {e}")))?;

    if !out.status.success() {
        return Err(SandboxError::Spawn(format!(
            "docker daemon not ready: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

pub fn docker_runtime() -> Arc<DockerRuntime> {
    Arc::new(DockerRuntime::from_env())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::io::IntegrationHandle;
    use std::path::PathBuf;
    use std::time::Duration;

    fn req(work: PathBuf, session: Uuid, backend: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            session_id: session,
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: work,
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            task_title: Some("demo".into()),
            task_summary: None,
            task_spec: Some("echo hi".into()),
            persona_name: None,
            persona_md: None,
            agent_backend: backend.map(str::to_string),
            agent_model: None,
            io: IntegrationHandle::none(),
        }
    }

    #[test]
    fn from_env_defaults_to_bridge_so_guests_can_heartbeat() {
        let prev = std::env::var("SANDBOX_DOCKER_NETWORK").ok();
        unsafe {
            std::env::remove_var("SANDBOX_DOCKER_NETWORK");
        }
        let rt = DockerRuntime::from_env();
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("SANDBOX_DOCKER_NETWORK", v);
            }
        }
        assert_ne!(
            rt.network, "none",
            "network=none drops /v1 heartbeats; default must be reachable"
        );
        assert_eq!(rt.network, "bridge");
    }

    #[tokio::test]
    async fn llm_backend_must_not_skip_container_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        tokio::fs::create_dir_all(&work).await.unwrap();
        let session = Uuid::new_v4();
        let rt = DockerRuntime::new("alpine:3.20", "bridge");
        match rt.spawn(req(work, session, Some("grok"))).await {
            Ok(h) => {
                let name = format!("fleet-sbx-{session}");
                let inspect = std::process::Command::new("docker")
                    .args(["inspect", "-f", "{{.Id}}", &name])
                    .output()
                    .expect("docker inspect");
                let _ = rt.destroy(&h).await;
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &name])
                    .status();
                assert!(
                    inspect.status.success(),
                    "agent_backend must not skip the container (AgentJob on the host is not isolation)"
                );
            }
            Err(e) => {
                let msg = e.to_string().to_ascii_lowercase();
                assert!(
                    msg.contains("docker"),
                    "missing docker must fail closed, not fall back to a host AgentJob: {e}"
                );
            }
        }
    }
}
