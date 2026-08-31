//! Local process sandbox: `bash fleet_worker.sh` in the session workdir.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::worker_script::materialize_workspace;
use super::{RuntimeHandle, SandboxError, SandboxRuntime, SandboxState, Signal, SpawnRequest};

struct ProcessEntry {
    project_id: Uuid,
    handle: RuntimeHandle,
    child: Option<Child>,
    agent: Option<super::agent_loop::AgentJob>,
    exit_code: Option<i32>,
    destroyed: bool,
}

/// Spawns `bash fleet_worker.sh` (or `commands.sh` via worker) in the session workdir.
pub struct ProcessRuntime {
    inner: Mutex<HashMap<String, ProcessEntry>>,
}

impl ProcessRuntime {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for ProcessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SandboxRuntime for ProcessRuntime {
    fn backend_name(&self) -> &'static str {
        "process"
    }

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError> {
        materialize_workspace(&req).await?;

        let script = req.work_dir.join("fleet_worker.sh");
        let mut cmd = Command::new("bash");
        cmd.arg(&script)
            .current_dir(&req.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null()) // worker tees to files itself
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env_clear();

        // Minimal env + FLEET_* only (no host secrets).
        cmd.env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        );
        cmd.env("HOME", &req.work_dir);
        cmd.env("TMPDIR", &req.work_dir);
        cmd.env("LANG", "C.UTF-8");
        for (k, v) in &req.env {
            if k.starts_with("FLEET_") {
                cmd.env(k, v);
            }
        }
        cmd.env("FLEET_WORK_DIR", req.work_dir.display().to_string());

        let child = cmd.spawn().map_err(|e| {
            SandboxError::Spawn(format!("process spawn failed (is bash installed?): {e}"))
        })?;

        let handle = RuntimeHandle {
            backend: "process".into(),
            id: req.session_id.to_string(),
        };

        let mut g = self.inner.lock().await;
        g.insert(
            handle.id.clone(),
            ProcessEntry {
                project_id: req.project_id,
                handle: handle.clone(),
                child: Some(child),
                agent: None,
                exit_code: None,
                destroyed: false,
            },
        );

        tracing::info!(
            session_id = %req.session_id,
            work_dir = %req.work_dir.display(),
            "process sandbox spawned"
        );
        Ok(handle)
    }

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError> {
        let mut g = self.inner.lock().await;
        let e = g
            .get_mut(&handle.id)
            .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
        if e.destroyed {
            return Ok(());
        }
        if let Some(child) = e.child.as_mut() {
            match signal {
                Signal::Term => {
                    #[cfg(unix)]
                    if let Some(pid) = child.id() {
                        let _ = nix_kill(pid, false);
                    }
                    #[cfg(not(unix))]
                    let _ = child.start_kill();
                }
                Signal::Kill => {
                    let _ = child.start_kill();
                }
            }
        }
        Ok(())
    }

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError> {
        let mut g = self.inner.lock().await;
        let Some(e) = g.get_mut(&handle.id) else {
            return Ok(());
        };
        if let Some(mut child) = e.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        if let Some(agent) = e.agent.take() {
            agent.abort();
        }
        e.destroyed = true;
        e.exit_code = e.exit_code.or(Some(0));
        Ok(())
    }

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError> {
        let mut g = self.inner.lock().await;
        let e = g
            .get_mut(&handle.id)
            .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;

        if e.destroyed {
            return Ok(SandboxState::Exited {
                code: e.exit_code.or(Some(0)),
            });
        }

        if let Some(code) = e.exit_code {
            return Ok(SandboxState::Exited { code: Some(code) });
        }

        if let Some(agent) = e.agent.as_ref() {
            if let Some(code) = agent.poll_exit() {
                e.exit_code = Some(code);
                e.agent = None;
                return Ok(SandboxState::Exited { code: Some(code) });
            }
            return Ok(SandboxState::Running);
        }

        if let Some(child) = e.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status
                        .code()
                        .unwrap_or(if status.success() { 0 } else { 1 });
                    e.exit_code = Some(code);
                    e.child = None;
                    return Ok(SandboxState::Exited { code: Some(code) });
                }
                Ok(None) => return Ok(SandboxState::Running),
                Err(err) => {
                    tracing::warn!(error = %err, id = %handle.id, "process try_wait failed");
                    return Ok(SandboxState::Unknown);
                }
            }
        }

        Ok(SandboxState::Exited {
            code: e.exit_code.or(Some(0)),
        })
    }

    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError> {
        let mut g = self.inner.lock().await;
        let mut live = Vec::new();
        for e in g.values_mut() {
            if e.project_id != project_id || e.destroyed {
                continue;
            }
            if let Some(child) = e.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    e.exit_code = Some(status.code().unwrap_or(1));
                    e.child = None;
                    continue;
                }
            } else if e.exit_code.is_some() {
                continue;
            }
            live.push(e.handle.clone());
        }
        Ok(live)
    }
}

#[cfg(unix)]
fn nix_kill(pid: u32, kill: bool) -> std::io::Result<()> {
    let sig = if kill { "-9" } else { "-15" };
    let status = std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("kill failed"))
    }
}

pub fn process_runtime() -> Arc<ProcessRuntime> {
    Arc::new(ProcessRuntime::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn process_runs_commands_sh() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_path_buf();
        let rt = ProcessRuntime::new();
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        let req = SpawnRequest {
            session_id: session,
            project_id: project,
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: work.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            task_title: Some("demo".into()),
            task_summary: Some("s".into()),
            task_spec: Some(
                r#"
Do it.

```bash
echo hello-from-sandbox > out.txt
```
"#
                .into(),
            ),
            persona_name: None,
            persona_md: None,
            agent_backend: None,
            agent_model: None,
            io: crate::sandbox::io::IntegrationHandle::none(),
        };
        let h = rt.spawn(req).await.unwrap();
        assert_eq!(h.backend, "process");

        for _ in 0..100 {
            match rt.state(&h).await.unwrap() {
                SandboxState::Exited { code } => {
                    assert_eq!(code, Some(0));
                    break;
                }
                SandboxState::Running | SandboxState::Unknown => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        let out = tokio::fs::read_to_string(work.join("out.txt"))
            .await
            .unwrap_or_default();
        assert!(out.contains("hello-from-sandbox"), "out={out}");
        rt.destroy(&h).await.unwrap();
    }

    #[tokio::test]
    async fn process_default_probe_exits_ok() {
        let dir = tempfile::tempdir().unwrap();
        let rt = ProcessRuntime::new();
        let h = rt
            .spawn(SpawnRequest {
                session_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                run_id: Uuid::new_v4(),
                board_task_id: Uuid::new_v4(),
                plan_task_id: "t1".into(),
                worker_profile: "coding".into(),
                worker_token: "tok".into(),
                work_dir: dir.path().to_path_buf(),
                env: HashMap::new(),
                timeout: Duration::from_secs(30),
                task_title: Some("probe".into()),
                task_summary: None,
                task_spec: Some("no commands".into()),
                persona_name: None,
                persona_md: None,
                agent_backend: None,
                agent_model: None,
                io: crate::sandbox::io::IntegrationHandle::none(),
            })
            .await
            .unwrap();
        for _ in 0..100 {
            if matches!(rt.state(&h).await.unwrap(), SandboxState::Exited { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(matches!(
            rt.state(&h).await.unwrap(),
            SandboxState::Exited { code: Some(0) }
        ));
    }

    #[tokio::test]
    async fn llm_backend_must_still_run_the_guest_script() {
        // Product: local sandbox guest is a process (fleet_worker.sh / fleet-driver),
        // not an in-process AgentJob. agent_backend must not skip the child.
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_path_buf();
        let rt = ProcessRuntime::new();
        let req = SpawnRequest {
            session_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: work.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            task_title: Some("demo".into()),
            task_summary: Some("s".into()),
            task_spec: Some(
                r#"
Do it.

```bash
echo hello-from-guest > out.txt
```
"#
                .into(),
            ),
            persona_name: None,
            persona_md: None,
            agent_backend: Some("grok".into()),
            agent_model: None,
            io: crate::sandbox::io::IntegrationHandle::none(),
        };
        let h = rt.spawn(req).await.unwrap();
        for _ in 0..80 {
            if matches!(rt.state(&h).await.unwrap(), SandboxState::Exited { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let out = tokio::fs::read_to_string(work.join("out.txt"))
            .await
            .unwrap_or_default();
        let _ = rt.destroy(&h).await;
        assert!(
            out.contains("hello-from-guest"),
            "agent_backend must not skip the guest script (got out={out:?})"
        );
    }
}
