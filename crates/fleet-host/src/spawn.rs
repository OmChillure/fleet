//! Spawn a leased job: process (host), container (docker / kube_pod), or vm.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use agent_fleet_contracts::ExecutorKind;
use anyhow::{Context, Result, anyhow, bail};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

use crate::client::{JobRuntime, SpawnPayload};
use crate::config::{ContainerBackend, LocalConfig, merge_child_env};
use crate::platform::{command_exists, hypervisor_on_path, in_cluster, kubeconfig_present};

const WORKER_TOKEN_KEY: &str = "FLEET_WORKER_TOKEN";
const HARNESS_KEY: &str = "FLEET_HARNESS";
pub const DOCKER_MISSING: &str =
    "docker not on PATH; container isolation will not fall back to host";

/// Guest binary: always `fleet-driver` (ACP). `FLEET_HARNESS` selects mock|grok.
pub fn host_child_bin<'a>(cfg: &'a LocalConfig, _env: &HashMap<String, String>) -> &'a str {
    cfg.fleet_driver_bin.as_str()
}

fn container_child_bin(cfg: &LocalConfig, env: &HashMap<String, String>) -> String {
    Path::new(host_child_bin(cfg, env))
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("fleet-driver")
        .to_string()
}

/// Drop host-absolute binary overrides. A container/kube guest cannot exec
/// `/home/…/mock-acp-agent`; it must use the image PATH / sibling lookup.
pub fn extra_env_for_isolated_guest(
    extra_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out = extra_env.clone();
    agent_fleet_contracts::strip_isolated_guest_env(&mut out);
    out
}

/// Job env for the child: payload.env + extra_env, then standard FLEET_* fills.
/// `extra_env` never sets or replaces `FLEET_WORKER_TOKEN`.
pub fn child_env(
    payload: &SpawnPayload,
    extra_env: &HashMap<String, String>,
    work_dir: &Path,
) -> HashMap<String, String> {
    let mut env = merge_child_env(&payload.env, extra_env);
    fill_if_absent(
        &mut env,
        "FLEET_SESSION_ID",
        &payload.session_id.to_string(),
    );
    fill_if_absent(
        &mut env,
        "FLEET_PROJECT_ID",
        &payload.project_id.to_string(),
    );
    fill_if_absent(&mut env, "FLEET_RUN_ID", &payload.run_id.to_string());
    fill_if_absent(
        &mut env,
        "FLEET_BOARD_TASK_ID",
        &payload.board_task_id.to_string(),
    );
    fill_if_absent(
        &mut env,
        "FLEET_TASK_ID",
        &payload.board_task_id.to_string(),
    );
    fill_if_absent(&mut env, "FLEET_PLAN_TASK_ID", &payload.plan_task_id);
    fill_if_absent(&mut env, "FLEET_WORKER_PROFILE", &payload.worker_profile);
    if !payload.orchestrator_url.is_empty() {
        fill_if_absent(
            &mut env,
            "FLEET_ORCHESTRATOR_URL",
            &payload.orchestrator_url,
        );
        fill_if_absent(&mut env, "FLEET_API_BASE", &payload.orchestrator_url);
    }
    if let Some(url) = payload.repo_url.as_deref().filter(|s| !s.is_empty()) {
        fill_if_absent(&mut env, "FLEET_REPO_URL", url);
    }
    if let Some(r) = payload.git_ref.as_deref().filter(|s| !s.is_empty()) {
        fill_if_absent(&mut env, "FLEET_GIT_REF", r);
    }
    if !env.contains_key(WORKER_TOKEN_KEY) && !payload.worker_token.is_empty() {
        env.insert(WORKER_TOKEN_KEY.into(), payload.worker_token.clone());
    }
    env.insert("FLEET_WORK_DIR".into(), work_dir.display().to_string());
    if env
        .get(HARNESS_KEY)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        env.insert(HARNESS_KEY.into(), "mock".into());
    }
    env
}

/// `child_env` then strip host-absolute `*_BIN`/`*_CMD` from the merged map
/// (payload.env ∪ extra_env). Isolated guests must not inherit plane paths.
pub fn child_env_for_isolated_guest(
    payload: &SpawnPayload,
    extra_env: &HashMap<String, String>,
    work_dir: &Path,
) -> HashMap<String, String> {
    extra_env_for_isolated_guest(&child_env(payload, extra_env, work_dir))
}

fn fill_if_absent(env: &mut HashMap<String, String>, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    env.entry(key.to_string())
        .or_insert_with(|| value.to_string());
}

/// What this process can see for executors. Unit tests inject a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolProbe {
    pub docker: bool,
    pub in_cluster: bool,
    pub kubeconfig: bool,
    pub hypervisor: bool,
}

impl ToolProbe {
    pub fn from_env() -> Self {
        Self {
            docker: command_exists("docker"),
            in_cluster: in_cluster(),
            kubeconfig: kubeconfig_present(),
            hypervisor: hypervisor_on_path(),
        }
    }

    pub fn container_available(self) -> bool {
        self.in_cluster || self.docker || self.kubeconfig
    }
}

pub fn pick_container_executor(cfg: &LocalConfig, probe: &ToolProbe) -> Result<ExecutorKind> {
    if probe.in_cluster {
        return Ok(ExecutorKind::KubePod);
    }
    match cfg.container_backend {
        ContainerBackend::Docker => {
            if probe.docker {
                Ok(ExecutorKind::Docker)
            } else {
                bail!("{DOCKER_MISSING}");
            }
        }
        ContainerBackend::K3d => {
            if probe.kubeconfig {
                Ok(ExecutorKind::KubePod)
            } else if probe.docker {
                Ok(ExecutorKind::Docker)
            } else {
                bail!(
                    "container isolation requires kube (in-cluster / KUBECONFIG) or docker; \
                     will not fall back to host"
                );
            }
        }
    }
}

pub fn runtime_allowed(
    runtime: JobRuntime,
    cfg: &LocalConfig,
    effective_host: bool,
    effective_container: bool,
) -> bool {
    runtime_allowed_with(
        runtime,
        cfg,
        effective_host,
        effective_container,
        &ToolProbe::from_env(),
    )
}

pub fn runtime_allowed_with(
    runtime: JobRuntime,
    cfg: &LocalConfig,
    effective_host: bool,
    effective_container: bool,
    probe: &ToolProbe,
) -> bool {
    match runtime {
        JobRuntime::Host => cfg.run_host && effective_host,
        JobRuntime::Container => cfg.run_container && effective_container,
        // Advertise / accept only when a hypervisor exists. Never fall back to Host.
        JobRuntime::Vm => probe.hypervisor,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveChild {
    Host { pid: u32 },
    Docker { name: String },
    Kube { name: String, namespace: String },
}

pub struct ChildRegistry {
    inner: Mutex<HashMap<Uuid, LiveChild>>,
}

impl ChildRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, id: Uuid, child: LiveChild) {
        self.inner.lock().await.insert(id, child);
    }

    pub async fn remove(&self, id: Uuid) -> Option<LiveChild> {
        self.inner.lock().await.remove(&id)
    }

    pub async fn get(&self, id: Uuid) -> Option<LiveChild> {
        self.inner.lock().await.get(&id).cloned()
    }

    pub async fn kill_session(&self, session_id: Uuid) -> Result<()> {
        let Some(handle) = self.inner.lock().await.remove(&session_id) else {
            return Ok(());
        };
        match handle {
            LiveChild::Host { pid } => sigterm_pid(pid),
            LiveChild::Docker { name } => docker_rm_f(&name).await,
            LiveChild::Kube { name, namespace } => {
                crate::exec_k8s::delete_sandbox(&name, &namespace).await
            }
        }
    }
}

impl Default for ChildRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static LIVE: OnceLock<ChildRegistry> = OnceLock::new();

pub fn live_children() -> &'static ChildRegistry {
    LIVE.get_or_init(ChildRegistry::new)
}

/// SIGTERM a host child, `docker rm -f`, or delete the FleetSandbox CR.
pub async fn kill_session(session_id: Uuid) -> Result<()> {
    live_children().kill_session(session_id).await
}

fn sigterm_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(anyhow!("SIGTERM {pid}: {err}"))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .status()
            .context("taskkill")?;
        if status.success() {
            Ok(())
        } else {
            bail!("taskkill {pid} failed");
        }
    }
}

fn materialize_workspace_files(session_dir: &Path, payload: &SpawnPayload) {
    let mut files: std::collections::BTreeMap<String, String> = payload
        .workspace_files
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(md) = payload
        .persona_md
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        files
            .entry("PERSONA.md".into())
            .or_insert_with(|| md.to_string());
    }
    if !files.is_empty() {
        let _ = agent_fleet_contracts::write_guest_files(session_dir, &files);
    }
    let _ = agent_fleet_contracts::ensure_fleet_tool(session_dir);
}

pub async fn run_claim(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    effective_host: bool,
    effective_container: bool,
) -> Result<()> {
    run_claim_with(
        cfg,
        payload,
        effective_host,
        effective_container,
        &ToolProbe::from_env(),
    )
    .await
}

pub async fn run_claim_with(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    effective_host: bool,
    effective_container: bool,
    probe: &ToolProbe,
) -> Result<()> {
    let runtime = payload.resolved_runtime()?;
    if let Some(mode) = payload
        .run_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let ok = matches!(
            (mode, runtime),
            ("on_pc", JobRuntime::Host) | ("in_container", JobRuntime::Container)
        );
        if !ok {
            bail!("run_mode {mode} does not match runtime {}", runtime.as_str());
        }
    }
    if !runtime_allowed_with(runtime, cfg, effective_host, effective_container, probe) {
        match runtime {
            JobRuntime::Vm => bail!(
                "vm isolation is not allowed (no firecracker/qemu on PATH); will not fall back to host"
            ),
            _ => bail!(
                "runtime {} is not allowed by local flags (run_host={}, run_container={}, backend={})",
                runtime.as_str(),
                cfg.run_host,
                cfg.run_container,
                cfg.container_backend.as_str()
            ),
        }
    }
    if payload.session_id.is_nil() {
        bail!("claim missing session_id");
    }
    if payload.privileged && !cfg.allow_privileged {
        bail!("privileged container spec refused (allow_privileged=false)");
    }
    if matches!(runtime, JobRuntime::Container | JobRuntime::Vm)
        && agent_fleet_contracts::url_is_loopback(&payload.orchestrator_url)
    {
        bail!(
            "loopback orchestrator URL {} is unreachable from a {} guest",
            payload.orchestrator_url,
            runtime.as_str()
        );
    }
    let container_exec = if runtime == JobRuntime::Container {
        Some(pick_container_executor(cfg, probe)?)
    } else {
        None
    };

    let session_dir = cfg.session_dir(payload.session_id);
    tokio::fs::create_dir_all(&session_dir)
        .await
        .with_context(|| format!("mkdir {}", session_dir.display()))?;
    if !payload.task_spec.is_empty() {
        let _ = tokio::fs::write(session_dir.join("spec.md"), &payload.task_spec).await;
        let _ = tokio::fs::write(session_dir.join("prompt.md"), &payload.task_spec).await;
    }
    if !payload.task_title.is_empty() {
        let _ = tokio::fs::write(session_dir.join("task.md"), &payload.task_title).await;
    }
    materialize_workspace_files(&session_dir, payload);

    let task_json = serde_json::json!({
        "title": payload.task_title,
        "spec": payload.task_spec,
        "session_id": payload.session_id,
    });
    if let Ok(body) = serde_json::to_vec_pretty(&task_json) {
        let _ = tokio::fs::write(session_dir.join("task.json"), body).await;
    }

    match runtime {
        JobRuntime::Host => run_host(cfg, payload, &session_dir).await,
        JobRuntime::Container => match container_exec.expect("container executor") {
            ExecutorKind::Docker => {
                docker_preflight_with(probe.docker)?;
                run_docker(cfg, payload, &session_dir).await
            }
            ExecutorKind::KubePod => {
                crate::exec_k8s::run_kube_pod(cfg, payload, &session_dir).await
            }
            other => bail!(
                "executor {other:?} cannot run container isolation; will not fall back to host"
            ),
        },
        JobRuntime::Vm => run_vm(cfg, payload, &session_dir).await,
    }
}

async fn run_host(cfg: &LocalConfig, payload: &SpawnPayload, cwd: &Path) -> Result<()> {
    let env = child_env(payload, &cfg.extra_env, cwd);
    let bin = host_child_bin(cfg, &env).to_string();

    let stdout = std::fs::File::create(cwd.join("stdout.log"))
        .with_context(|| format!("stdout.log in {}", cwd.display()))?;
    let stderr = std::fs::File::create(cwd.join("stderr.log"))
        .with_context(|| format!("stderr.log in {}", cwd.display()))?;

    let mut cmd = Command::new(&bin);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .envs(&env);

    if !cfg.use_host_git_credentials {
        cmd.env("HOME", cwd);
        cmd.env_remove("SSH_AUTH_SOCK");
        cmd.env_remove("GIT_ASKPASS");
        cmd.env_remove("GIT_SSH_COMMAND");
    }

    info!(bin = %bin, cwd = %cwd.display(), "spawn host worker");
    let mut child = cmd.spawn().with_context(|| format!("exec {bin}"))?;
    if let Some(pid) = child.id() {
        live_children()
            .insert(payload.session_id, LiveChild::Host { pid })
            .await;
    }
    let result = wait_child(&mut child, payload.timeout_s, &bin).await;
    live_children().remove(payload.session_id).await;
    result
}

pub fn docker_preflight_with(docker_on_path: bool) -> Result<()> {
    if !docker_on_path {
        bail!("{DOCKER_MISSING}");
    }
    Ok(())
}

pub fn docker_container_name(session_id: Uuid) -> String {
    format!("fleet-wh-{session_id}")
}

pub fn docker_run_args(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    session_dir: &Path,
    container_name: &str,
) -> Vec<String> {
    let env = child_env_for_isolated_guest(payload, &cfg.extra_env, Path::new("/work"));
    let bin = container_child_bin(cfg, &env);
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "-w".into(),
        "/work".into(),
        "-v".into(),
        format!("{}:/work", session_dir.display()),
    ];
    // Image USER is 10001; bind-mounted host work dir is owned by the
    // operator. Run as the host uid so result.md is writable.
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        args.push("--user".into());
        args.push(format!("{uid}:{gid}"));
    }
    let net = cfg.docker_network.trim();
    let net = if net.is_empty() || net.eq_ignore_ascii_case("none") {
        "bridge"
    } else {
        net
    };
    args.push("--network".into());
    args.push(net.to_string());
    if payload.privileged && cfg.allow_privileged {
        args.push("--privileged".into());
    }
    for (k, v) in &env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push(cfg.docker_image.clone());
    args.push(bin);
    args
}

async fn run_docker(cfg: &LocalConfig, payload: &SpawnPayload, session_dir: &Path) -> Result<()> {
    docker_preflight_with(command_exists("docker"))?;
    let name = docker_container_name(payload.session_id);
    let args = docker_run_args(cfg, payload, session_dir, &name);

    let stdout = std::fs::File::create(session_dir.join("stdout.log"))
        .with_context(|| format!("stdout.log in {}", session_dir.display()))?;
    let stderr = std::fs::File::create(session_dir.join("stderr.log"))
        .with_context(|| format!("stderr.log in {}", session_dir.display()))?;

    info!(image = %cfg.docker_image, container = %name, "docker run");
    live_children()
        .insert(payload.session_id, LiveChild::Docker { name: name.clone() })
        .await;

    let mut cmd = Command::new("docker");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    let result = match cmd.spawn().context("docker run") {
        Ok(mut child) => wait_child(&mut child, payload.timeout_s, "docker").await,
        Err(e) => Err(e),
    };
    live_children().remove(payload.session_id).await;
    let _ = docker_rm_f(&name).await;
    result
}

async fn docker_rm_f(name: &str) -> Result<()> {
    if !command_exists("docker") {
        bail!("docker not on PATH; cannot rm {name}");
    }
    let out = Command::new("docker")
        .args(["rm", "-f", name])
        .output()
        .await
        .context("docker rm")?;
    if !out.status.success() {
        let msg = tail_stdio(&out.stdout, &out.stderr);
        if msg.contains("No such container") {
            return Ok(());
        }
        bail!("docker rm {name}: {msg}");
    }
    Ok(())
}

/// Read `result.md` from the host session dir (or synthesize it from
/// `artifacts/` for older guest images). None if the guest left no proof.
pub fn collect_guest_proof(session_dir: &Path) -> Option<String> {
    let direct = std::fs::read_to_string(session_dir.join("result.md")).ok();
    if let Some(md) = direct.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(md.to_string());
    }
    let transcript = std::fs::read_to_string(session_dir.join("artifacts").join("transcript.md"))
        .unwrap_or_default();
    let has_json = session_dir.join("artifacts").join("result.json").is_file();
    if transcript.trim().is_empty() && !has_json {
        return None;
    }
    let mut md = String::from("# Result\n\nACP completed.\n\n");
    if !transcript.trim().is_empty() {
        md.push_str("## Transcript\n\n");
        md.push_str(transcript.trim());
        md.push('\n');
    }
    Some(md)
}

/// POST `/v1/.../status` with `result_md` so the plane can validate_done.
/// Lease complete is not proof.
pub async fn publish_guest_proof(payload: &SpawnPayload, session_dir: &Path) -> Result<()> {
    let Some(md) = collect_guest_proof(session_dir) else {
        return Ok(());
    };
    if payload.orchestrator_url.is_empty() || payload.worker_token.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{}/v1/projects/{}/workers/{}/status",
        payload.orchestrator_url.trim_end_matches('/'),
        payload.project_id,
        payload.session_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("proof http client")?;
    let packed = agent_fleet_contracts::collect_guest_files(session_dir);
    let files = agent_fleet_contracts::files_as_json_map(&packed);
    let files = if files.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(files)
    };
    let res = client
        .post(&url)
        .bearer_auth(&payload.worker_token)
        .json(&serde_json::json!({
            "seq": 90,
            "status": "proof",
            "reason": "guest proof",
            "result_md": md,
            "files": files,
        }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        bail!("publish guest proof {status}: {}", text.trim());
    }
    Ok(())
}

/// Fail-closed VM path. Hypervisor on PATH is required; never falls back to host.
pub async fn run_vm(cfg: &LocalConfig, payload: &SpawnPayload, session_dir: &Path) -> Result<()> {
    run_vm_with(cfg, payload, session_dir, hypervisor_on_path()).await
}

fn vm_kernel_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("FLEET_VM_KERNEL") {
        let p = std::path::PathBuf::from(p.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    for cand in ["/boot/vmlinuz", "/boot/vmlinuz-linux"] {
        let p = std::path::Path::new(cand);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    None
}

fn vm_rootfs_path() -> Option<std::path::PathBuf> {
    let raw = std::env::var("FLEET_VM_ROOTFS").ok()?;
    let p = std::path::PathBuf::from(raw.trim());
    p.is_file().then_some(p)
}

fn qemu_bin() -> Option<&'static str> {
    for b in [
        "qemu-system-x86_64",
        "qemu-system-aarch64",
        "firecracker",
    ] {
        if command_exists(b) {
            return Some(b);
        }
    }
    None
}

/// QEMU argv for a 9p-mounted session dir. Fail-closed: no host exec.
pub fn qemu_vm_args(
    qemu: &str,
    kernel: &Path,
    rootfs: &Path,
    session_dir: &Path,
    timeout_s: u64,
) -> Vec<String> {
    let _ = timeout_s;
    vec![
        qemu.into(),
        "-nographic".into(),
        "-no-reboot".into(),
        "-m".into(),
        "512".into(),
        "-kernel".into(),
        kernel.display().to_string(),
        "-drive".into(),
        format!("file={},format=raw,if=virtio", rootfs.display()),
        "-fsdev".into(),
        format!(
            "local,id=fsdev0,path={},security_model=mapped",
            session_dir.display()
        ),
        "-device".into(),
        "virtio-9p-pci,fsdev=fsdev0,mount_tag=work".into(),
        "-append".into(),
        "console=ttyS0 root=/dev/vda rw".into(),
    ]
}

async fn run_vm_with(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    session_dir: &Path,
    hypervisor: bool,
) -> Result<()> {
    let _ = cfg;
    if !hypervisor {
        bail!("vm isolation requires firecracker or qemu on PATH; will not fall back to host");
    }
    let Some(qemu) = qemu_bin() else {
        bail!("vm isolation requires firecracker or qemu on PATH; will not fall back to host");
    };
    let Some(kernel) = vm_kernel_path() else {
        bail!(
            "vm isolation requires FLEET_VM_KERNEL (or /boot/vmlinuz); will not fall back to host"
        );
    };
    let Some(rootfs) = vm_rootfs_path() else {
        bail!("vm isolation requires FLEET_VM_ROOTFS; will not fall back to host");
    };
    let args = qemu_vm_args(qemu, &kernel, &rootfs, session_dir, payload.timeout_s);
    let stdout = std::fs::File::create(session_dir.join("stdout.log"))
        .with_context(|| format!("stdout.log in {}", session_dir.display()))?;
    let stderr = std::fs::File::create(session_dir.join("stderr.log"))
        .with_context(|| format!("stderr.log in {}", session_dir.display()))?;
    info!(bin = qemu, cwd = %session_dir.display(), "spawn vm worker");
    let mut cmd = Command::new(qemu);
    cmd.args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| format!("exec {qemu}"))?;
    if let Some(pid) = child.id() {
        live_children()
            .insert(payload.session_id, LiveChild::Host { pid })
            .await;
    }
    let result = wait_child(&mut child, payload.timeout_s, qemu).await;
    live_children().remove(payload.session_id).await;
    result
}

async fn wait_child(child: &mut tokio::process::Child, timeout_s: u64, name: &str) -> Result<()> {
    let timeout = Duration::from_secs(timeout_s.max(1));
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                Ok(())
            } else {
                Err(anyhow!("{name} exited {status}"))
            }
        }
        Ok(Err(e)) => Err(e).with_context(|| format!("wait {name}")),
        Err(_) => {
            let _ = child.kill().await;
            Err(anyhow!("{name} timed out after {}s", timeout.as_secs()))
        }
    }
}

fn tail_stdio(stdout: &[u8], stderr: &[u8]) -> String {
    let mut s = String::new();
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    let out = String::from_utf8_lossy(stdout).trim().to_string();
    if !err.is_empty() {
        s.push_str(&err);
    }
    if !out.is_empty() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&out);
    }
    if s.len() > 2000 {
        s.truncate(2000);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_vm_args_mount_session_dir_and_never_exec_host_bin() {
        let args = qemu_vm_args(
            "qemu-system-x86_64",
            Path::new("/boot/vmlinuz"),
            Path::new("/var/lib/fleet/rootfs.img"),
            Path::new("/tmp/session"),
            60,
        );
        let joined = args.join(" ");
        assert!(joined.contains("-nographic"));
        assert!(joined.contains("mount_tag=work"));
        assert!(joined.contains("/tmp/session"));
        assert!(!joined.contains("fleet-driver"));
        assert!(!joined.contains("run_host"));
    }

    #[test]
    fn child_env_defaults_harness_to_mock() {
        let env = child_env(&sample_payload(), &HashMap::new(), Path::new("/work"));
        assert_eq!(env.get(HARNESS_KEY).map(String::as_str), Some("mock"));
    }

    #[test]
    fn child_env_keeps_extra_env_harness() {
        let mut extra = HashMap::new();
        extra.insert(HARNESS_KEY.into(), "grok".into());
        let env = child_env(&sample_payload(), &extra, Path::new("/work"));
        assert_eq!(env.get(HARNESS_KEY).map(String::as_str), Some("grok"));
    }

    #[test]
    fn child_env_defaults_blank_or_whitespace_harness_to_mock() {
        for blank in ["", " \t "] {
            let mut extra = HashMap::new();
            extra.insert(HARNESS_KEY.into(), blank.into());
            let env = child_env(&sample_payload(), &extra, Path::new("/work"));
            assert_eq!(
                env.get(HARNESS_KEY).map(String::as_str),
                Some("mock"),
                "extra FLEET_HARNESS={blank:?}"
            );

            let mut payload = sample_payload();
            payload.env.insert(HARNESS_KEY.into(), blank.into());
            let env = child_env(&payload, &HashMap::new(), Path::new("/work"));
            assert_eq!(
                env.get(HARNESS_KEY).map(String::as_str),
                Some("mock"),
                "payload.env FLEET_HARNESS={blank:?}"
            );
        }
    }

    #[test]
    fn child_env_keeps_payload_env_harness() {
        let mut payload = sample_payload();
        payload.env.insert(HARNESS_KEY.into(), "grok".into());
        let env = child_env(&payload, &HashMap::new(), Path::new("/work"));
        assert_eq!(env.get(HARNESS_KEY).map(String::as_str), Some("grok"));
    }

    fn sample_payload() -> SpawnPayload {
        SpawnPayload {
            kind: String::new(),
            session_id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            run_id: uuid::Uuid::nil(),
            board_task_id: uuid::Uuid::nil(),
            plan_task_id: String::new(),
            worker_profile: String::new(),
            worker_token: String::new(),
            orchestrator_url: String::new(),
            runtime: JobRuntime::Host,
            run_mode: None,
            isolation: None,
            task_title: String::new(),
            task_spec: String::new(),
            timeout_s: 3600,
            repo_url: None,
            git_ref: None,
            env: HashMap::new(),
            privileged: false,
            persona_md: None,
            workspace_files: HashMap::new(),
        }
    }

    fn no_tools() -> ToolProbe {
        ToolProbe {
            docker: false,
            in_cluster: false,
            kubeconfig: false,
            hypervisor: false,
        }
    }

    #[test]
    fn child_env_overwrites_payload_fleet_work_dir() {
        let mut payload = sample_payload();
        payload
            .env
            .insert("FLEET_WORK_DIR".into(), "/control/plane/path".into());
        let env = child_env(&payload, &HashMap::new(), Path::new("/work"));
        assert_eq!(env.get("FLEET_WORK_DIR").map(String::as_str), Some("/work"));
    }

    #[test]
    fn vm_not_allowed_without_hypervisor() {
        let cfg = LocalConfig::defaults();
        assert!(cfg.run_host);
        let probe = no_tools();
        assert!(!probe.hypervisor);
        assert!(
            !runtime_allowed_with(JobRuntime::Vm, &cfg, true, true, &probe),
            "Vm must stay closed when run_host/effective_* are true but hypervisor is missing"
        );
    }

    #[test]
    fn vm_allowed_with_hypervisor_when_host_and_container_flags_off() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_host = false;
        cfg.run_container = false;
        let probe = ToolProbe {
            docker: false,
            in_cluster: false,
            kubeconfig: false,
            hypervisor: true,
        };
        assert!(
            runtime_allowed_with(JobRuntime::Vm, &cfg, false, false, &probe),
            "Vm is probe.hypervisor only; run_host/run_container/effective_* must not gate it"
        );
    }

    #[test]
    fn vm_not_allowed_when_only_container_tools_present() {
        let cfg = LocalConfig::defaults();
        let probe = ToolProbe {
            docker: true,
            in_cluster: true,
            kubeconfig: true,
            hypervisor: false,
        };
        assert!(
            !runtime_allowed_with(JobRuntime::Vm, &cfg, true, true, &probe),
            "docker/in_cluster/kubeconfig must not stand in for hypervisor"
        );
    }

    #[test]
    fn in_cluster_picks_kube_pod() {
        let cfg = LocalConfig::defaults();
        let probe = ToolProbe {
            docker: true,
            in_cluster: true,
            kubeconfig: false,
            hypervisor: false,
        };
        assert_eq!(
            pick_container_executor(&cfg, &probe).unwrap(),
            ExecutorKind::KubePod
        );
    }

    #[test]
    fn pick_container_executor_k3d_uses_docker_when_kubeconfig_absent() {
        let cfg = LocalConfig::defaults();
        assert_eq!(cfg.container_backend, ContainerBackend::K3d);
        let probe = ToolProbe {
            docker: true,
            in_cluster: false,
            kubeconfig: false,
            hypervisor: false,
        };
        assert_eq!(
            pick_container_executor(&cfg, &probe).unwrap(),
            ExecutorKind::Docker
        );
    }

    #[test]
    fn docker_args_use_bridge_network_and_fleet_env() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.session_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        p.worker_token = "tok".into();
        let args = docker_run_args(&cfg, &p, Path::new("/tmp/sess"), "fleet-wh-test");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--network" && w[1] == "bridge"),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "FLEET_WORKER_TOKEN=tok"),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "-e"
                && w[1] == "FLEET_SESSION_ID=550e8400-e29b-41d4-a716-446655440000"),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "FLEET_HARNESS=mock"),
            "{args:?}"
        );
    }

    #[test]
    fn docker_run_args_empty_or_whitespace_network_becomes_bridge() {
        for blank in ["", "  "] {
            let mut cfg = LocalConfig::defaults();
            cfg.docker_network = blank.into();
            let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
            let i = args
                .iter()
                .position(|a| a == "--network")
                .expect("--network");
            assert_eq!(
                args.get(i + 1).map(String::as_str),
                Some("bridge"),
                "docker_network={blank:?}: {args:?}"
            );
        }
    }

    #[test]
    fn docker_run_args_custom_network_follows_network_flag() {
        let mut cfg = LocalConfig::defaults();
        cfg.docker_network = "labnet".into();
        let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
        let i = args
            .iter()
            .position(|a| a == "--network")
            .expect("--network");
        assert_eq!(args[i], "--network");
        assert_eq!(args[i + 1], "labnet", "{args:?}");
    }

    #[test]
    fn docker_run_args_command_is_basename_of_absolute_driver_bin() {
        let mut cfg = LocalConfig::defaults();
        cfg.fleet_driver_bin = "/opt/fleet/fleet-driver".into();
        let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
        assert_eq!(
            args.last().map(String::as_str),
            Some("fleet-driver"),
            "container command must be the basename, not the host path: {args:?}"
        );
    }

    #[test]
    fn isolated_guest_drops_host_absolute_bin_overrides() {
        let mut extra = HashMap::new();
        extra.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/home/viny/.local/bin/mock-acp-agent".into(),
        );
        extra.insert("FLEET_HARNESS".into(), "mock".into());
        extra.insert("FLEET_GROK_BIN".into(), "grok".into());
        let mut cfg = LocalConfig::defaults();
        cfg.extra_env = extra;
        let mut payload = sample_payload();
        payload.env.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/opt/plane/mock-acp-agent".into(),
        );
        payload
            .env
            .insert("FLEET_WORK_CMD".into(), "echo host-only".into());
        let args = docker_run_args(&cfg, &payload, Path::new("/tmp/sess"), "n");
        assert!(
            !args
                .iter()
                .any(|a| a.contains("/home/viny/.local/bin/mock-acp-agent")),
            "container must use image mock-acp-agent: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("/opt/plane/mock-acp-agent")),
            "plane payload.env host path must not ride into docker: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|w| w[0] == "-e" && w[1].starts_with("FLEET_WORK_CMD=")),
            "FLEET_WORK_CMD is host-only: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "FLEET_HARNESS=mock"),
            "relative FLEET_HARNESS must still appear as -e: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "FLEET_GROK_BIN=grok"),
            "relative FLEET_GROK_BIN must still appear as -e: {args:?}"
        );
    }

    #[test]
    fn host_child_env_keeps_absolute_bin_overrides() {
        let mut extra = HashMap::new();
        extra.insert(
            "FLEET_MOCK_ACP_BIN".into(),
            "/home/viny/.local/bin/mock-acp-agent".into(),
        );
        let env = child_env(&sample_payload(), &extra, Path::new("/tmp/sess"));
        assert_eq!(
            env.get("FLEET_MOCK_ACP_BIN").map(String::as_str),
            Some("/home/viny/.local/bin/mock-acp-agent"),
            "run_host must still honor operator host paths: {env:?}"
        );
    }

    #[test]
    fn docker_run_args_drops_loopback_extra_env() {
        let mut cfg = LocalConfig::defaults();
        cfg.extra_env
            .insert("FLEET_API_BASE".into(), "http://127.0.0.1:8080".into());
        let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
        assert!(
            !args.iter().any(|a| a.contains("127.0.0.1")),
            "loopback extra_env must not ride into docker: {args:?}"
        );
    }

    #[test]
    fn materialize_writes_team_and_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut payload = sample_payload();
        payload
            .workspace_files
            .insert("TEAM.md".into(), "you are t1\n".into());
        payload
            .workspace_files
            .insert("work/app.rs".into(), "fn main() {}\n".into());
        payload.persona_md = Some("# coder\n".into());
        materialize_workspace_files(dir.path(), &payload);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TEAM.md")).unwrap(),
            "you are t1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/app.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("PERSONA.md"))
                .unwrap()
                .contains("coder")
        );
        assert!(dir.path().join("fleet-tool.sh").is_file());
    }

    #[test]
    fn collect_guest_proof_prefers_result_md() {
        let dir = tempfile::tempdir().unwrap();
        let result_md = "# Result\n\nACP end_turn from guest.\n";
        std::fs::write(dir.path().join("result.md"), result_md).unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/transcript.md"),
            "transcript-only body from artifacts\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"ok":true}"#,
        )
        .unwrap();
        let md = collect_guest_proof(dir.path()).expect("proof");
        assert_eq!(md, result_md.trim());
        assert!(
            !md.contains("ACP completed"),
            "synthesis header must not win over result.md: {md}"
        );
        assert!(
            !md.contains("transcript-only body from artifacts"),
            "artifacts/transcript.md must not leak when result.md is non-empty: {md}"
        );
    }

    #[test]
    fn collect_guest_proof_empty_or_whitespace_result_md_falls_through_to_transcript() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), " \n\t").unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/transcript.md"),
            "hello from mock agent\n",
        )
        .unwrap();
        let md = collect_guest_proof(dir.path()).expect("whitespace result.md must not be None");
        assert!(md.contains("hello from mock agent"), "{md}");
        assert!(md.contains("# Result"), "{md}");
        assert_eq!(
            md,
            "# Result\n\nACP completed.\n\n## Transcript\n\nhello from mock agent\n"
        );
    }

    #[test]
    fn collect_guest_proof_json_only_synthesizes_header() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"ok":true}"#,
        )
        .unwrap();
        let md = collect_guest_proof(dir.path()).expect("json-only proof");
        assert_eq!(md, "# Result\n\nACP completed.\n\n");
    }

    #[test]
    fn collect_guest_proof_synthesizes_from_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/transcript.md"),
            "hello from mock agent\n",
        )
        .unwrap();
        let md = collect_guest_proof(dir.path()).expect("synth");
        assert!(md.contains("hello from mock agent"), "{md}");
        assert!(md.contains("# Result"), "{md}");
    }

    #[test]
    fn collect_guest_proof_none_without_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect_guest_proof(dir.path()).is_none());
    }

    #[test]
    fn docker_network_none_is_remapped_to_bridge() {
        let mut cfg = LocalConfig::defaults();
        cfg.docker_network = "none".into();
        let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
        let i = args
            .iter()
            .position(|a| a == "--network")
            .expect("--network");
        assert_eq!(
            args.get(i + 1).map(String::as_str),
            Some("bridge"),
            "docker_network=none remaps to --network bridge so guests can heartbeat: {args:?}"
        );
    }

    #[test]
    fn docker_run_args_none_any_case_or_padded_becomes_bridge() {
        for none in ["NONE", " None "] {
            let mut cfg = LocalConfig::defaults();
            cfg.docker_network = none.into();
            let args = docker_run_args(&cfg, &sample_payload(), Path::new("/tmp/sess"), "n");
            let i = args
                .iter()
                .position(|a| a == "--network")
                .expect("--network");
            assert_eq!(
                args.get(i + 1).map(String::as_str),
                Some("bridge"),
                "docker_network={none:?}: {args:?}"
            );
        }
    }

    #[tokio::test]
    async fn container_claim_refuses_loopback_orchestrator_url() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.container_backend = ContainerBackend::Docker;
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.orchestrator_url = "http://127.0.0.1:8080".into();
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.to_ascii_lowercase().contains("loopback"),
            "container guests cannot reach the host's localhost: {err}"
        );
    }

    #[tokio::test]
    async fn run_claim_refuses_privileged_when_not_allowed() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.allow_privileged = false;
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.privileged = true;
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.to_ascii_lowercase().contains("privileged"),
            "privileged must fail closed, not silently drop the flag: {err}"
        );
    }

    #[tokio::test]
    async fn run_claim_does_not_refuse_privileged_when_allowed() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.allow_privileged = true;
        cfg.container_backend = ContainerBackend::Docker;
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.privileged = true;
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            !err.to_ascii_lowercase().contains("privileged"),
            "allow_privileged=true must not refuse the privileged spec: {err}"
        );
        assert!(
            err.contains("docker"),
            "next fail after the allow gate is docker-missing, not privileged: {err}"
        );
    }

    #[test]
    fn docker_args_omit_privileged_when_not_allowed() {
        let cfg = LocalConfig::defaults();
        assert!(!cfg.allow_privileged);
        let mut p = sample_payload();
        p.privileged = true;
        let args = docker_run_args(&cfg, &p, Path::new("/tmp/sess"), "n");
        assert!(
            !args.iter().any(|a| a == "--privileged"),
            "allow_privileged=false must omit --privileged: {args:?}"
        );
    }

    #[test]
    fn docker_args_privileged_only_when_allowed() {
        let mut cfg = LocalConfig::defaults();
        cfg.allow_privileged = true;
        let mut p = sample_payload();
        p.privileged = true;
        let args = docker_run_args(&cfg, &p, Path::new("/tmp/sess"), "n");
        assert!(
            args.iter().any(|a| a == "--privileged"),
            "privileged=true and allow_privileged=true must pass --privileged: {args:?}"
        );
    }

    #[test]
    fn docker_args_omit_privileged_when_payload_false_even_if_allowed() {
        let mut cfg = LocalConfig::defaults();
        cfg.allow_privileged = true;
        let mut p = sample_payload();
        p.privileged = false;
        let args = docker_run_args(&cfg, &p, Path::new("/tmp/sess"), "n");
        assert!(
            !args.iter().any(|a| a == "--privileged"),
            "payload.privileged=false must not emit --privileged even when allow_privileged: {args:?}"
        );
    }

    #[tokio::test]
    async fn run_claim_container_docker_missing() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.container_backend = ContainerBackend::Docker;
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            err.contains(DOCKER_MISSING),
            "Docker backend without docker must publish DOCKER_MISSING: {err}"
        );
        assert!(
            err.contains("docker not on PATH"),
            "must name the missing tool, not a generic docker token: {err}"
        );
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            !lower.contains("docker run"),
            "must fail before exec docker: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a docker/host child"
        );
    }

    #[tokio::test]
    async fn run_claim_k3d_without_kube_or_docker_does_not_fall_back_to_host() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        assert_eq!(cfg.container_backend, ContainerBackend::K3d);
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            err.contains("will not fall back"),
            "K3d with neither kube nor docker must not fall back: {err}"
        );
        assert!(
            err.contains("KUBECONFIG"),
            "K3d neither-tool branch, not Docker-only DOCKER_MISSING: {err}"
        );
        assert!(
            !err.contains("docker not on PATH"),
            "default K3d must not use the Docker-backend token: {err}"
        );
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a host child"
        );
    }

    #[tokio::test]
    async fn run_claim_docker_backend_does_not_use_kubeconfig_when_docker_missing() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.container_backend = ContainerBackend::Docker;
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.session_id = Uuid::now_v7();
        let probe = ToolProbe {
            docker: false,
            in_cluster: false,
            kubeconfig: true,
            hypervisor: false,
        };
        let err = run_claim_with(&cfg, &p, true, true, &probe)
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            err.contains(DOCKER_MISSING),
            "Docker backend must not fall through to KubePod when docker is missing: {err}"
        );
        assert!(
            !lower.contains("k8s"),
            "must not pick KubePod: {err}"
        );
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a kube/docker/host child"
        );
    }

    #[tokio::test]
    async fn run_claim_container_runtime_rejected_when_run_container_false() {
        let cfg = LocalConfig::defaults();
        assert!(!cfg.run_container);
        assert_eq!(cfg.container_backend, ContainerBackend::K3d);
        let mut p = sample_payload();
        p.runtime = JobRuntime::Container;
        p.isolation = Some("container".into());
        p.session_id = Uuid::now_v7();
        let tools = ToolProbe {
            docker: true,
            in_cluster: false,
            kubeconfig: true,
            hypervisor: false,
        };
        let err = run_claim_with(&cfg, &p, true, true, &tools)
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(err.contains("not allowed"), "{err}");
        assert!(err.contains("run_container"), "{err}");
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            !lower.contains("docker run")
                && !lower.contains("k3d cluster")
                && !lower.contains("docker not on path"),
            "must fail on flags before exec docker/k3d: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a docker/k3d child"
        );
    }

    #[tokio::test]
    async fn run_claim_vm_without_hypervisor_bails() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.session_id = Uuid::now_v7();
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            err.contains("not allowed"),
            "must fail the advertise/allow gate, not a later path: {err}"
        );
        assert!(
            err.contains("no firecracker/qemu"),
            "must name the missing hypervisor, not a generic flags bail: {err}"
        );
        assert!(
            err.contains("will not fall back"),
            "must refuse Host fallback: {err}"
        );
        assert!(
            !lower.contains("not implemented"),
            "must not reach run_vm: {err}"
        );
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a host child"
        );
    }

    #[tokio::test]
    async fn run_claim_vm_without_hypervisor_does_not_mkdir_session_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = LocalConfig::defaults();
        cfg.work_dir = tmp.path().display().to_string();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.session_id = Uuid::now_v7();
        let session_dir = cfg.session_dir(p.session_id);
        let err = run_claim_with(&cfg, &p, true, true, &no_tools())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not allowed") && err.contains("no firecracker/qemu"),
            "must fail the advertise gate before mkdir: {err}"
        );
        assert!(
            !session_dir.exists(),
            "advertise-gate bail must leave session_dir absent: {}",
            session_dir.display()
        );
    }

    #[tokio::test]
    async fn run_claim_vm_refuses_loopback_orchestrator_url() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.orchestrator_url = "http://127.0.0.1:8080".into();
        p.session_id = Uuid::now_v7();
        let probe = ToolProbe {
            docker: false,
            in_cluster: false,
            kubeconfig: false,
            hypervisor: true,
        };
        let err = run_claim_with(&cfg, &p, true, true, &probe)
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            lower.contains("loopback"),
            "vm guests cannot reach the host's localhost: {err}"
        );
        assert!(
            !lower.contains("not implemented"),
            "must fail at the loopback gate, not inside run_vm: {err}"
        );
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not register a host child"
        );
    }

    #[tokio::test]
    async fn run_claim_vm_with_injected_hypervisor_does_not_spawn_host() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.session_id = Uuid::now_v7();
        let probe = ToolProbe {
            docker: false,
            in_cluster: false,
            kubeconfig: false,
            hypervisor: true,
        };
        let err = run_claim_with(&cfg, &p, true, true, &probe)
            .await
            .unwrap_err()
            .to_string();
        let lower = err.to_ascii_lowercase();
        assert!(
            err.contains("FLEET_VM_ROOTFS")
                || err.contains("FLEET_VM_KERNEL")
                || err.contains("requires firecracker or qemu"),
            "must fail inside run_vm (missing hypervisor/kernel/rootfs), not the allow-gate: {err}"
        );
        assert!(!err.contains("not allowed"), "{err}");
        assert!(
            !lower.contains("spawn host"),
            "must not fall back to host: {err}"
        );
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "must not insert LiveChild::Host"
        );
    }

    #[tokio::test]
    async fn run_vm_fails_closed() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.session_id = Uuid::now_v7();
        let session_dir = tempfile::tempdir().unwrap();
        for hypervisor in [false, true] {
            let err = run_vm_with(&cfg, &p, session_dir.path(), hypervisor)
                .await
                .unwrap_err()
                .to_string();
            let lower = err.to_ascii_lowercase();
            assert!(
                !lower.contains("spawn host"),
                "hypervisor={hypervisor} must not fall back to host: {err}"
            );
            assert!(
                !session_dir.path().join("stdout.log").exists()
                    && !session_dir.path().join("stderr.log").exists(),
                "hypervisor={hypervisor} must not start a host child"
            );
            assert!(
                live_children().get(p.session_id).await.is_none(),
                "hypervisor={hypervisor} must not register a live child"
            );
        }
    }

    #[tokio::test]
    async fn run_vm_does_not_register_live_child() {
        let cfg = LocalConfig::defaults();
        let mut p = sample_payload();
        p.runtime = JobRuntime::Vm;
        p.isolation = Some("vm".into());
        p.session_id = Uuid::now_v7();
        let _ = run_vm_with(&cfg, &p, Path::new("/tmp"), true)
            .await
            .unwrap_err();
        assert!(
            live_children().get(p.session_id).await.is_none(),
            "run_vm must not insert a live child"
        );
    }

    #[tokio::test]
    async fn kill_session_missing_is_ok() {
        let id = Uuid::now_v7();
        kill_session(id).await.expect("missing child is idempotent");
    }

    #[tokio::test]
    async fn kill_session_sigterms_host_child() {
        let mut child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep");
        let pid = child.id().expect("pid");
        let id = Uuid::now_v7();
        live_children().insert(id, LiveChild::Host { pid }).await;
        kill_session(id).await.expect("sigterm");
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("wait after SIGTERM")
            .expect("status");
        assert!(!status.success(), "SIGTERM should stop sleep: {status}");
        assert!(live_children().get(id).await.is_none());
    }
}
