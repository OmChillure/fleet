//! Process sandbox backend (PR7): exec + process group + pidfile.
//!
//! WorkDir = DATA_DIR/projects/{project}/sessions/{session}
//! Pidfile  = WorkDir/pid
//!
//! Unix only. Spawns `PROCESS_WORKER_BIN` (default `fleet-driver`) with the
//! FLEET_* env contract. The guest is `fleet-driver` (ACP + heartbeat/status).

use super::{RuntimeHandle, SandboxError, SandboxRuntime, SandboxState, Signal, SpawnRequest};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use uuid::Uuid;

const PIDFILE: &str = "pid";

/// Real process backend.
pub struct ProcessRuntime {
    data_dir: PathBuf,
    /// argv[0] = program; remaining args are prefixed before spawn.
    worker_argv: Vec<String>,
}

impl ProcessRuntime {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        worker_argv: Vec<String>,
    ) -> Result<Self, SandboxError> {
        if worker_argv.is_empty() {
            return Err(SandboxError::Spawn(
                "PROCESS_WORKER_BIN / worker argv is empty".into(),
            ));
        }
        Ok(Self {
            data_dir: data_dir.into(),
            worker_argv,
        })
    }

    fn session_dir(&self, project_id: Uuid, session_id: Uuid) -> PathBuf {
        self.data_dir
            .join("projects")
            .join(project_id.to_string())
            .join("sessions")
            .join(session_id.to_string())
    }

    fn work_dir_for_req(&self, req: &SpawnRequest) -> PathBuf {
        // Prefer orchestrator-provided work_dir (absolute); fall back to convention.
        if req.work_dir.as_os_str().is_empty() {
            self.session_dir(req.project_id, req.session_id)
        } else {
            req.work_dir.clone()
        }
    }

    fn pidfile_path(work_dir: &Path) -> PathBuf {
        work_dir.join(PIDFILE)
    }

    async fn write_pidfile(work_dir: &Path, pid: u32) -> Result<(), SandboxError> {
        let path = Self::pidfile_path(work_dir);
        tokio::fs::write(&path, format!("{pid}\n"))
            .await
            .map_err(|e| SandboxError::Spawn(format!("pidfile write: {e}")))
    }

    async fn read_pidfile(work_dir: &Path) -> Option<i32> {
        let bytes = tokio::fs::read_to_string(Self::pidfile_path(work_dir))
            .await
            .ok()?;
        bytes.trim().parse().ok()
    }

    fn session_id_from_handle(handle: &RuntimeHandle) -> Result<Uuid, SandboxError> {
        Uuid::parse_str(&handle.id)
            .map_err(|_| SandboxError::NotFound(format!("bad handle id {}", handle.id)))
    }

    /// Resolve work_dir for an existing handle by scanning project session dirs,
    /// or using the conventional path when project_id is known from List context.
    async fn find_work_dir(&self, session_id: Uuid) -> Option<PathBuf> {
        let projects = self.data_dir.join("projects");
        let mut dirs = tokio::fs::read_dir(&projects).await.ok()?;
        while let Ok(Some(entry)) = dirs.next_entry().await {
            let sess = entry.path().join("sessions").join(session_id.to_string());
            if Self::pidfile_path(&sess).exists() || sess.exists() {
                return Some(sess);
            }
        }
        None
    }

    fn signal_pgid(pid: i32, sig: Signal) -> Result<(), SandboxError> {
        #[cfg(unix)]
        {
            let signo = match sig {
                Signal::Term => libc::SIGTERM,
                Signal::Kill => libc::SIGKILL,
            };
            // Negative pid = process group (spawn used process_group(0)).
            let rc = unsafe { libc::kill(-pid, signo) };
            if rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            // Fallback: signal the process itself if group kill fails.
            let rc2 = unsafe { libc::kill(pid, signo) };
            if rc2 == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(SandboxError::Signal(format!(
                    "kill({pid}): {}",
                    std::io::Error::last_os_error()
                )))
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, sig);
            Err(SandboxError::Signal(
                "process sandbox is only supported on Unix".into(),
            ))
        }
    }

    fn process_alive(pid: i32) -> bool {
        #[cfg(unix)]
        {
            // signal 0 = existence check
            let rc = unsafe { libc::kill(pid, 0) };
            rc == 0
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }

    fn wait_nonblock_exit_code(pid: i32) -> Option<i32> {
        #[cfg(unix)]
        {
            let mut status: libc::c_int = 0;
            let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if r == pid {
                if libc::WIFEXITED(status) {
                    return Some(libc::WEXITSTATUS(status));
                }
                if libc::WIFSIGNALED(status) {
                    return Some(128 + libc::WTERMSIG(status));
                }
                return Some(0);
            }
            None
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }
}

#[async_trait]
impl SandboxRuntime for ProcessRuntime {
    fn backend_name(&self) -> &'static str {
        "process"
    }

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError> {
        #[cfg(not(unix))]
        {
            let _ = req;
            return Err(SandboxError::Spawn(
                "process sandbox is only supported on Unix".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            let work_dir = self.work_dir_for_req(&req);
            tokio::fs::create_dir_all(&work_dir)
                .await
                .map_err(|e| SandboxError::Spawn(format!("create work_dir: {e}")))?;

            // Optional spec dump for workers that read files.
            if !req.plan_task_id.is_empty() {
                let _ =
                    tokio::fs::write(work_dir.join("plan_task_id.txt"), &req.plan_task_id).await;
            }

            let program = &self.worker_argv[0];
            let mut cmd = Command::new(program);
            if self.worker_argv.len() > 1 {
                cmd.args(&self.worker_argv[1..]);
            }
            let stdout = std::fs::File::create(work_dir.join("stdout.log"))
                .map_err(|e| SandboxError::Spawn(format!("stdout log: {e}")))?;
            let stderr = std::fs::File::create(work_dir.join("stderr.log"))
                .map_err(|e| SandboxError::Spawn(format!("stderr log: {e}")))?;
            cmd.current_dir(&work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .env_clear()
                .env(
                    "PATH",
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
                )
                .env("HOME", &work_dir)
                .env("TMPDIR", &work_dir)
                .env("LANG", "C.UTF-8");
            for (k, v) in &req.env {
                if k.starts_with("FLEET_") {
                    cmd.env(k, v);
                }
            }

            // New process group so we can signal the whole tree.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }

            let child = cmd
                .spawn()
                .map_err(|e| SandboxError::Spawn(format!("exec {program}: {e}")))?;
            let pid = child.id();
            // Leave the process running; do not wait/kill on Drop (std::Child Drop
            // does not kill). Reap later via waitpid(WNOHANG) in state/destroy.
            std::mem::forget(child);

            Self::write_pidfile(&work_dir, pid).await?;

            Ok(RuntimeHandle {
                backend: "process".into(),
                id: req.session_id.to_string(),
            })
        }
    }

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError> {
        let session_id = Self::session_id_from_handle(handle)?;
        let work_dir = self
            .find_work_dir(session_id)
            .await
            .ok_or_else(|| SandboxError::NotFound(handle.id.clone()))?;
        let Some(pid) = Self::read_pidfile(&work_dir).await else {
            return Ok(()); // nothing to signal
        };
        Self::signal_pgid(pid, signal)
    }

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError> {
        let session_id = match Self::session_id_from_handle(handle) {
            Ok(id) => id,
            Err(_) => return Ok(()), // unknown handle = success
        };
        let Some(work_dir) = self.find_work_dir(session_id).await else {
            return Ok(());
        };
        if let Some(pid) = Self::read_pidfile(&work_dir).await {
            let _ = Self::signal_pgid(pid, Signal::Term);
            // brief grace then force
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if Self::process_alive(pid) {
                let _ = Self::signal_pgid(pid, Signal::Kill);
            }
            let _ = Self::wait_nonblock_exit_code(pid);
        }
        let _ = tokio::fs::remove_file(Self::pidfile_path(&work_dir)).await;
        Ok(())
    }

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError> {
        let session_id = Self::session_id_from_handle(handle)?;
        let Some(work_dir) = self.find_work_dir(session_id).await else {
            return Ok(SandboxState::Unknown);
        };
        let Some(pid) = Self::read_pidfile(&work_dir).await else {
            return Ok(SandboxState::Exited { code: None });
        };
        if let Some(code) = Self::wait_nonblock_exit_code(pid) {
            return Ok(SandboxState::Exited { code: Some(code) });
        }
        if Self::process_alive(pid) {
            Ok(SandboxState::Running)
        } else {
            // Process gone but we never reaped — treat as exited unknown code.
            Ok(SandboxState::Exited { code: None })
        }
    }

    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError> {
        let sessions_root = self
            .data_dir
            .join("projects")
            .join(project_id.to_string())
            .join("sessions");
        let mut out = Vec::new();
        let mut dirs = match tokio::fs::read_dir(&sessions_root).await {
            Ok(d) => d,
            Err(_) => return Ok(out),
        };
        while let Ok(Some(entry)) = dirs.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let pidfile = Self::pidfile_path(&path);
            if !pidfile.exists() {
                continue;
            }
            let Some(session_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(session_id) = Uuid::parse_str(session_name) else {
                continue;
            };
            if out.iter().any(|h| h.id == session_id.to_string()) {
                continue;
            }
            if let Some(pid) = Self::read_pidfile(&path).await {
                if Self::process_alive(pid) || Self::wait_nonblock_exit_code(pid).is_none() {
                    // alive or zombie not yet fully gone with pidfile present
                    if Self::process_alive(pid) {
                        out.push(RuntimeHandle {
                            backend: "process".into(),
                            id: session_id.to_string(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Parse `PROCESS_WORKER_BIN` env: either a single path or shell-ish argv joined
/// by whitespace (no quotes). Default: `fleet-driver` on PATH.
pub fn worker_argv_from_env() -> Vec<String> {
    match std::env::var("PROCESS_WORKER_BIN") {
        Ok(s) if !s.trim().is_empty() => s.split_whitespace().map(|p| p.to_string()).collect(),
        _ => vec!["fleet-driver".into()],
    }
}

/// Build process runtime from data_dir + env worker bin.
pub fn build_process_runtime(
    data_dir: impl Into<PathBuf>,
) -> Result<Arc<ProcessRuntime>, SandboxError> {
    Ok(Arc::new(ProcessRuntime::new(
        data_dir,
        worker_argv_from_env(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{SandboxRuntime, fleet_worker_env};
    use std::collections::HashMap;
    use std::time::Duration;

    fn sleep_argv() -> Vec<String> {
        // Portable enough for unit tests on macOS/Linux.
        vec!["/bin/sleep".into(), "30".into()]
    }

    fn true_argv() -> Vec<String> {
        // macOS has /usr/bin/true; Linux often /bin/true — use shell for portability.
        vec!["/bin/sh".into(), "-c".into(), "exit 0".into()]
    }

    fn sample_req(session: Uuid, project: Uuid, work_dir: PathBuf) -> SpawnRequest {
        let mut req = SpawnRequest {
            session_id: session,
            project_id: project,
            run_id: Uuid::new_v4(),
            board_task_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: work_dir.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            task_title: None,
            task_summary: None,
            task_spec: None,
            persona_name: None,
            persona_md: None,
            agent_backend: None,
            agent_model: None,
            io: crate::sandbox::io::IntegrationHandle::none(),
        };
        req.env = fleet_worker_env(&req, "http://127.0.0.1:8080");
        req
    }

    #[tokio::test]
    async fn process_spawn_running_signal_destroy() {
        let dir = tempfile::tempdir().unwrap();
        let rt = ProcessRuntime::new(dir.path(), sleep_argv()).unwrap();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let work = dir
            .path()
            .join("projects")
            .join(project.to_string())
            .join("sessions")
            .join(session.to_string());
        tokio::fs::create_dir_all(&work).await.unwrap();

        let h = rt
            .spawn(sample_req(session, project, work.clone()))
            .await
            .unwrap();
        assert_eq!(h.backend, "process");
        assert_eq!(h.id, session.to_string());
        assert!(work.join("pid").exists());

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(rt.state(&h).await.unwrap(), SandboxState::Running);

        let list = rt.list(project).await.unwrap();
        assert_eq!(list.len(), 1);

        rt.signal(&h, Signal::Term).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // after term, sleep should exit
        match rt.state(&h).await.unwrap() {
            SandboxState::Exited { .. } | SandboxState::Unknown => {}
            SandboxState::Running => {
                rt.destroy(&h).await.unwrap();
            }
        }
        rt.destroy(&h).await.unwrap();
        // second destroy idempotent
        rt.destroy(&h).await.unwrap();
    }

    #[tokio::test]
    async fn process_true_exits_quickly() {
        let dir = tempfile::tempdir().unwrap();
        let rt = ProcessRuntime::new(dir.path(), true_argv()).unwrap();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let work = dir
            .path()
            .join("projects")
            .join(project.to_string())
            .join("sessions")
            .join(session.to_string());
        tokio::fs::create_dir_all(&work).await.unwrap();
        let h = rt.spawn(sample_req(session, project, work)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        match rt.state(&h).await.unwrap() {
            SandboxState::Exited { code } => {
                assert!(code.unwrap_or(0) == 0 || code.is_none());
            }
            other => panic!("expected exited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn process_spawn_missing_worker_bin_errors_without_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-fleet-driver");
        assert!(
            !missing.exists(),
            "fixture must not exist: {}",
            missing.display()
        );
        let rt =
            ProcessRuntime::new(dir.path(), vec![missing.to_string_lossy().into_owned()]).unwrap();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let work = dir
            .path()
            .join("projects")
            .join(project.to_string())
            .join("sessions")
            .join(session.to_string());
        tokio::fs::create_dir_all(&work).await.unwrap();

        let err = match rt.spawn(sample_req(session, project, work.clone())).await {
            Ok(h) => panic!(
                "missing PROCESS_WORKER_BIN must not spawn a handle (got {h:?}); \
                 no silent stub"
            ),
            Err(e) => e,
        };
        match err {
            SandboxError::Spawn(msg) => {
                assert!(
                    msg.contains("exec"),
                    "spawn error must name the exec, got {msg}"
                );
                assert!(
                    msg.contains("no-such-fleet-driver"),
                    "spawn error must name the missing bin, got {msg}"
                );
            }
            other => panic!("expected SandboxError::Spawn, got {other:?}"),
        }
        assert!(
            !work.join("pid").exists(),
            "missing worker bin must not write a pidfile (that would be a silent stub)"
        );
        assert!(
            rt.list(project).await.unwrap().is_empty(),
            "missing worker bin must not list a running process session"
        );
    }

    #[tokio::test]
    async fn process_spawn_with_agent_backend_still_execs_child() {
        let dir = tempfile::tempdir().unwrap();
        let rt = ProcessRuntime::new(dir.path(), true_argv()).unwrap();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let work = dir
            .path()
            .join("projects")
            .join(project.to_string())
            .join("sessions")
            .join(session.to_string());
        tokio::fs::create_dir_all(&work).await.unwrap();
        let mut req = sample_req(session, project, work.clone());
        req.agent_backend = Some("grok".into());
        let h = rt.spawn(req).await.unwrap();
        assert_eq!(h.backend, "process");
        assert!(
            work.join("pid").exists(),
            "agent_backend must not skip the child (no AgentJob)"
        );
    }
}
