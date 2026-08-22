//! Spawn a leased job on the host or in a container / k3d pod.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::process::Command;
use tracing::{info, warn};

use crate::client::{JobRuntime, SpawnPayload};
use crate::config::{ContainerBackend, LocalConfig, merge_child_env};
use crate::platform::command_exists;

const WORKER_TOKEN_KEY: &str = "FLEET_WORKER_TOKEN";
const HARNESS_KEY: &str = "FLEET_HARNESS";

/// Host binary: `fleet-driver` when `FLEET_HARNESS` is set and non-empty.
pub fn host_child_bin<'a>(cfg: &'a LocalConfig, env: &HashMap<String, String>) -> &'a str {
    let harness = env
        .get(HARNESS_KEY)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    if harness.is_some() {
        cfg.fleet_driver_bin.as_str()
    } else {
        cfg.fleet_worker_bin.as_str()
    }
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
    env
}

fn fill_if_absent(env: &mut HashMap<String, String>, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    env.entry(key.to_string())
        .or_insert_with(|| value.to_string());
}

pub fn runtime_allowed(
    runtime: JobRuntime,
    cfg: &LocalConfig,
    effective_host: bool,
    effective_container: bool,
) -> bool {
    match runtime {
        JobRuntime::Host => cfg.run_host && effective_host,
        // v1: container / k3d is not a working executor. Refuse even if the
        // operator flipped run_container and docker/k3d is on PATH.
        JobRuntime::Container => {
            let _ = (cfg, effective_container);
            false
        }
    }
}

pub async fn run_claim(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    effective_host: bool,
    effective_container: bool,
) -> Result<()> {
    if !runtime_allowed(payload.runtime, cfg, effective_host, effective_container) {
        bail!(
            "runtime {} is not allowed by local flags (run_host={}, run_container={}, backend={})",
            payload.runtime.as_str(),
            cfg.run_host,
            cfg.run_container,
            cfg.container_backend.as_str()
        );
    }
    if payload.session_id.is_nil() {
        bail!("claim missing session_id");
    }
    if payload.privileged && !cfg.allow_privileged {
        bail!("privileged container spec refused (allow_privileged=false)");
    }

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
    let task_json = serde_json::json!({
        "title": payload.task_title,
        "spec": payload.task_spec,
        "session_id": payload.session_id,
    });
    if let Ok(body) = serde_json::to_vec_pretty(&task_json) {
        let _ = tokio::fs::write(session_dir.join("task.json"), body).await;
    }

    match payload.runtime {
        JobRuntime::Host => run_host(cfg, payload, &session_dir).await,
        JobRuntime::Container => {
            bail!("container/k3d runtime is not supported in v1")
        }
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
    wait_child(&mut child, payload.timeout_s, &bin).await
}

#[allow(dead_code)] // kept for the later container/k3d executor; v1 refuses above
async fn run_container(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    session_dir: &Path,
) -> Result<()> {
    match cfg.container_backend {
        ContainerBackend::Docker => {
            docker_run(cfg, payload, session_dir, cfg.allow_privileged).await
        }
        ContainerBackend::K3d => k3d_run(cfg, payload, session_dir).await,
    }
}

async fn docker_run(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    session_dir: &Path,
    allow_privileged: bool,
) -> Result<()> {
    if !command_exists("docker") {
        bail!("docker not on PATH");
    }
    let env = child_env(payload, &cfg.extra_env, Path::new("/work"));
    let mut args: Vec<String> = vec!["run".into(), "--rm".into()];
    let net = cfg.docker_network.trim();
    if !net.is_empty() {
        args.push("--network".into());
        args.push(net.to_string());
    }
    args.extend([
        "-w".into(),
        "/work".into(),
        "-v".into(),
        format!("{}:/work", session_dir.display()),
    ]);
    if payload.privileged && allow_privileged {
        args.push("--privileged".into());
    }
    for (k, v) in &env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push(cfg.docker_image.clone());
    args.push("fleet-worker".into());

    info!(image = %cfg.docker_image, "docker run one-shot");
    let mut cmd = Command::new("docker");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("docker run")?;
    wait_child(&mut child, payload.timeout_s, "docker").await
}

async fn k3d_run(cfg: &LocalConfig, payload: &SpawnPayload, session_dir: &Path) -> Result<()> {
    if !command_exists("k3d") {
        bail!("k3d not on PATH");
    }
    ensure_k3d_cluster(cfg).await?;
    if command_exists("kubectl") {
        match kubectl_run(cfg, payload).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(error = %e, "kubectl run failed; trying docker");
                if command_exists("docker") {
                    return docker_run(cfg, payload, session_dir, cfg.allow_privileged).await;
                }
                return Err(e);
            }
        }
    }
    if command_exists("docker") {
        return docker_run(cfg, payload, session_dir, cfg.allow_privileged).await;
    }
    bail!("k3d cluster is up but neither kubectl nor docker is on PATH");
}

async fn ensure_k3d_cluster(cfg: &LocalConfig) -> Result<()> {
    if k3d_has_cluster(&cfg.k3d_cluster).await? {
        return Ok(());
    }
    info!(cluster = %cfg.k3d_cluster, agents = cfg.k3d_agents, "k3d cluster create");
    let mut cmd = Command::new("k3d");
    cmd.args([
        "cluster",
        "create",
        &cfg.k3d_cluster,
        "--agents",
        &cfg.k3d_agents.to_string(),
        "--k3s-arg",
        "--disable=traefik@server:*",
    ]);
    if !cfg.k3d_memory.trim().is_empty() {
        cmd.args(["--servers-memory", cfg.k3d_memory.trim()]);
    }
    let out = cmd.output().await.context("k3d cluster create")?;
    if !out.status.success() {
        bail!(
            "k3d cluster create failed: {}",
            tail_stdio(&out.stdout, &out.stderr)
        );
    }
    Ok(())
}

async fn k3d_has_cluster(name: &str) -> Result<bool> {
    let json = Command::new("k3d")
        .args(["cluster", "list", "-o", "json"])
        .output()
        .await;
    if let Ok(out) = json {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if cluster_name_in_list(text.trim(), name) {
                return Ok(true);
            }
            // Valid empty JSON list.
            if text.trim().starts_with('[') || text.trim().starts_with('{') {
                return Ok(false);
            }
        }
    }
    let out = Command::new("k3d")
        .args(["cluster", "list"])
        .output()
        .await
        .context("k3d cluster list")?;
    if !out.status.success() {
        bail!(
            "k3d cluster list failed: {}",
            tail_stdio(&out.stdout, &out.stderr)
        );
    }
    Ok(cluster_name_in_list(
        &String::from_utf8_lossy(&out.stdout),
        name,
    ))
}

fn cluster_name_in_list(stdout: &str, name: &str) -> bool {
    let trimmed = stdout.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return json_cluster_names(&v).iter().any(|n| n == name);
    }
    stdout.lines().skip(1).any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|col| col == name)
    })
}

fn json_cluster_names(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(n) = item.get("name").and_then(|x| x.as_str()) {
                    out.push(n.to_string());
                }
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(n) = map.get("name").and_then(|x| x.as_str()) {
                out.push(n.to_string());
            }
            if let Some(arr) = map.get("clusters").and_then(|x| x.as_array()) {
                for item in arr {
                    if let Some(n) = item.get("name").and_then(|x| x.as_str()) {
                        out.push(n.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

async fn kubectl_run(cfg: &LocalConfig, payload: &SpawnPayload) -> Result<()> {
    let kubeconfig = k3d_kubeconfig(&cfg.k3d_cluster).await?;
    let env = child_env(payload, &cfg.extra_env, Path::new("/work"));
    let pod = format!("fleet-wh-{}", payload.session_id);
    let context = format!("k3d-{}", cfg.k3d_cluster);

    let mut args: Vec<String> = vec![
        "--kubeconfig".into(),
        kubeconfig.clone(),
        "--context".into(),
        context,
        "run".into(),
        pod.clone(),
        "--image".into(),
        cfg.docker_image.clone(),
        "--restart=Never".into(),
        "--command".into(),
    ];
    for (k, v) in &env {
        args.push(format!("--env={k}={v}"));
    }
    args.push("--".into());
    args.push("fleet-worker".into());

    info!(pod = %pod, image = %cfg.docker_image, "kubectl run one-shot");
    let out = Command::new("kubectl")
        .args(&args)
        .output()
        .await
        .context("kubectl run")?;
    if !out.status.success() {
        bail!(
            "kubectl run failed: {}",
            tail_stdio(&out.stdout, &out.stderr)
        );
    }

    let timeout = format!("{}s", payload.timeout_s.max(1));
    let wait = Command::new("kubectl")
        .args([
            "--kubeconfig",
            &kubeconfig,
            "wait",
            "--for=jsonpath={.status.phase}=Succeeded",
            &format!("pod/{pod}"),
            &format!("--timeout={timeout}"),
        ])
        .output()
        .await
        .context("kubectl wait")?;

    let ok = wait.status.success();
    if !ok {
        let logs = Command::new("kubectl")
            .args(["--kubeconfig", &kubeconfig, "logs", &pod])
            .output()
            .await;
        let log_txt = logs
            .ok()
            .map(|o| tail_stdio(&o.stdout, &o.stderr))
            .unwrap_or_default();
        let _ = Command::new("kubectl")
            .args([
                "--kubeconfig",
                &kubeconfig,
                "delete",
                "pod",
                &pod,
                "--ignore-not-found",
            ])
            .output()
            .await;
        bail!(
            "kubectl pod {pod} did not succeed: {} {}",
            tail_stdio(&wait.stdout, &wait.stderr),
            log_txt
        );
    }

    let _ = Command::new("kubectl")
        .args([
            "--kubeconfig",
            &kubeconfig,
            "delete",
            "pod",
            &pod,
            "--ignore-not-found",
        ])
        .output()
        .await;
    Ok(())
}

async fn k3d_kubeconfig(cluster: &str) -> Result<String> {
    let out = Command::new("k3d")
        .args(["kubeconfig", "write", cluster])
        .output()
        .await
        .context("k3d kubeconfig write")?;
    if !out.status.success() {
        bail!(
            "k3d kubeconfig write failed: {}",
            tail_stdio(&out.stdout, &out.stderr)
        );
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        bail!("k3d kubeconfig write returned empty path");
    }
    Ok(path)
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
    use crate::config::merge_child_env;

    #[test]
    fn extra_env_cannot_replace_fleet_worker_token() {
        let mut payload = HashMap::new();
        payload.insert(WORKER_TOKEN_KEY.into(), "job-token".into());
        let mut extra = HashMap::new();
        extra.insert(WORKER_TOKEN_KEY.into(), "from-extra".into());
        extra.insert("FLEET_HARNESS".into(), "mock".into());
        let merged = merge_child_env(&payload, &extra);
        assert_eq!(merged.get(WORKER_TOKEN_KEY).unwrap(), "job-token");
        assert_eq!(merged.get("FLEET_HARNESS").unwrap(), "mock");
        let cfg = LocalConfig::defaults();
        assert_eq!(host_child_bin(&cfg, &merged), cfg.fleet_driver_bin);
        let mut no_harness = merged.clone();
        no_harness.remove("FLEET_HARNESS");
        assert_eq!(host_child_bin(&cfg, &no_harness), cfg.fleet_worker_bin);
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
            task_title: String::new(),
            task_spec: String::new(),
            timeout_s: 3600,
            repo_url: None,
            git_ref: None,
            env: HashMap::new(),
            privileged: false,
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
    fn v1_refuses_container_runtime_even_if_local_flag_is_on() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        assert!(runtime_allowed(JobRuntime::Host, &cfg, true, true));
        assert!(
            !runtime_allowed(JobRuntime::Container, &cfg, true, true),
            "container/k3d must not be a v1 executor"
        );
    }
}
