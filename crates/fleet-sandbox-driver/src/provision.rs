//! Workspace provisioning.
//!
//! Modes:
//! - **volume** (default when workspace agent present): clone + project_setup
//!   run in the **workspace** container via shared-volume handshake
//!   (`/workspace/.fleet/request.json` → status/result). Harness image is not mutated.
//! - **local**: clone (and optional commands) inside the harness process
//!   (unit tests / single-container fallback).

use crate::handshake;
use crate::job::Job;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Volume provision is single-flight per driver process (handshake files).
static PROVISION_LOCK: Mutex<()> = Mutex::new(());

pub struct Provisioned {
    pub work_dir: PathBuf,
    pub repo_url: String,
    pub git_ref: String,
    pub milestones: Vec<String>,
    pub mode: String,
    pub detail: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FleetRequest {
    job_id: String,
    action: String,
    #[serde(default)]
    req_id: String,
    repo_url: String,
    git_ref: String,
    work_dir: String,
    commands: Vec<String>,
    smoke_commands: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
    /// Named tools written into the job tree for fleet-wsexec allowlist.
    #[serde(default)]
    tools: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hmac: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FleetStatus {
    phase: Option<String>,
    ok: Option<bool>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FleetResult {
    ok: bool,
    work_dir: Option<String>,
    error: Option<String>,
    milestones: Option<Vec<String>>,
}

pub fn fleet_dir() -> PathBuf {
    PathBuf::from(std::env::var("FLEET_DIR").unwrap_or_else(|_| "/workspace/.fleet".into()))
}

/// Prefer volume handoff when workspace agent is enabled (lab default).
pub fn provision_mode() -> String {
    std::env::var("FLEET_PROVISION_MODE")
        .unwrap_or_else(|_| "volume".into())
        .to_ascii_lowercase()
}

pub fn provision(job: &Job) -> Result<Provisioned> {
    let repo_url = job
        .repo_url
        .clone()
        .or_else(|| std::env::var("GIT_REPO_URL").ok())
        .context("repo_url missing in job and GIT_REPO_URL env")?;

    let git_ref = if job.git_ref.is_empty() {
        std::env::var("GIT_REF").unwrap_or_else(|_| "main".into())
    } else {
        job.git_ref.clone()
    };

    let work_dir = crate::policy::resolve_work_dir(job.job_id.as_str(), job.work_dir.as_deref());
    crate::policy::validate_job_cwd(&work_dir).with_context(|| {
        format!(
            "job work_dir outside isolation: {} (use /workspace/jobs/{{job_id}}/repo)",
            work_dir.display()
        )
    })?;

    let mode = provision_mode();
    match mode.as_str() {
        "local" => provision_local(job, &repo_url, &git_ref, &work_dir),
        _ => {
            let _guard = PROVISION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            provision_via_workspace_agent(job, &repo_url, &git_ref, &work_dir)
        }
    }
}

fn setup_commands(
    job: &Job,
) -> (
    Vec<String>,
    Vec<String>,
    std::collections::BTreeMap<String, String>,
) {
    let Some(ps) = &job.project_setup else {
        return (vec![], vec![], Default::default());
    };
    let mut commands = ps.commands.clone();
    if ps.strategy == "script" {
        if let Some(script) = &ps.script {
            commands = vec![format!("bash -lc '{}'", script.replace('\'', "'\\''"))];
            // Prefer executing relative script if it looks like a path
            if !script.contains(' ') {
                commands = vec![format!("bash {}", script)];
            }
        }
    }
    (commands, ps.smoke_commands.clone(), ps.env.clone())
}

fn provision_via_workspace_agent(
    job: &Job,
    repo_url: &str,
    git_ref: &str,
    work_dir: &Path,
) -> Result<Provisioned> {
    let fleet = fleet_dir();
    fs::create_dir_all(&fleet).context("create .fleet dir on shared volume")?;

    // Wait briefly for workspace agent to appear (status file or just dir writable)
    wait_for_workspace_agent(&fleet, Duration::from_secs(60))?;

    let (commands, smoke, mut setup_env) = setup_commands(job);
    for (k, v) in &job.env_public {
        setup_env.insert(k.clone(), v.clone());
    }
    let tools_json = job
        .project_setup
        .as_ref()
        .map(|p| serde_json::to_value(&p.tools).unwrap_or(json!({})))
        .unwrap_or(json!({}));
    let timeout = job
        .project_setup
        .as_ref()
        .and_then(|p| p.timeout_sec)
        .unwrap_or_else(|| {
            std::env::var("FLEET_PROVISION_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(900)
        });

    let req_id = uuid::Uuid::new_v4().to_string();
    let req = FleetRequest {
        job_id: job.job_id.clone(),
        action: "provision".into(),
        req_id: req_id.clone(),
        repo_url: repo_url.to_string(),
        git_ref: git_ref.to_string(),
        work_dir: work_dir.display().to_string(),
        commands,
        smoke_commands: smoke,
        env: setup_env,
        tools: tools_json,
        hmac: None,
    };
    let mut value = serde_json::to_value(&req)?;
    value = handshake::sign_value(value)?;
    let req_path = fleet.join(format!("request-{req_id}.json"));
    let tmp = fleet.join(format!("request-{req_id}.json.tmp"));
    fs::write(&tmp, serde_json::to_string(&value)?)?;
    fs::rename(&tmp, &req_path).context("write signed provision request")?;
    info!(path = %req_path.display(), %req_id, "provision request written for workspace agent");

    let result_path = fleet.join(format!("result-{req_id}.json"));
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut last_phase = String::new();
    loop {
        if Instant::now() > deadline {
            bail!("workspace provision timed out after {timeout}s (last_phase={last_phase})");
        }
        if let Ok(raw) = fs::read_to_string(fleet.join("status.json")) {
            if let Ok(st) = serde_json::from_str::<FleetStatus>(&raw) {
                if let Some(phase) = &st.phase {
                    if phase != &last_phase {
                        tracing::debug!(%phase, message = ?st.message, "workspace provision progress");
                        last_phase = phase.clone();
                    }
                    if phase == "failed" || st.ok == Some(false) {
                        let err = st
                            .message
                            .unwrap_or_else(|| "workspace provision failed".into());
                        bail!("{err}");
                    }
                }
            }
        }
        if result_path.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    let raw = fs::read_to_string(&result_path).context("read provision result")?;
    let value: serde_json::Value = serde_json::from_str(&raw).context("parse result.json")?;
    handshake::verify_value(&value).context("unsigned or forged provision result (hmac)")?;
    let result: FleetResult = serde_json::from_value(value).context("decode result.json")?;
    if !result.ok {
        bail!(
            "workspace provision failed: {}",
            result.error.unwrap_or_else(|| raw)
        );
    }
    let milestones = result.milestones.unwrap_or_default();
    info!(?milestones, "workspace provision complete");
    let chosen = accept_agent_work_dir(result.work_dir, work_dir)?;
    Ok(Provisioned {
        work_dir: chosen,
        repo_url: repo_url.to_string(),
        git_ref: git_ref.to_string(),
        milestones,
        mode: "volume".into(),
        detail: Some(serde_json::from_str(&raw).unwrap_or(json!({}))),
    })
}

/// Trust the agent's reported work_dir only if it stays under jobs_root().
fn accept_agent_work_dir(reported: Option<String>, requested: &Path) -> Result<PathBuf> {
    let chosen = reported
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| requested.to_path_buf());
    crate::policy::validate_job_cwd(&chosen).with_context(|| {
        format!(
            "workspace agent returned work_dir outside isolation: {}",
            chosen.display()
        )
    })?;
    Ok(chosen)
}

fn wait_for_workspace_agent(fleet: &Path, max: Duration) -> Result<()> {
    let start = Instant::now();
    let marker = fleet.join("agent.alive");
    while start.elapsed() < max {
        if marker.is_file() {
            info!("workspace agent alive");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "workspace agent not ready within {:?} (missing {}); is the workspace service running?",
        max,
        marker.display()
    )
}

fn provision_local(
    job: &Job,
    repo_url: &str,
    git_ref: &str,
    work_dir: &Path,
) -> Result<Provisioned> {
    let needs_ssh = repo_url.starts_with("git@") || repo_url.starts_with("ssh://");
    if needs_ssh {
        let key =
            std::env::var("GIT_SSH_KEY_PATH").unwrap_or_else(|_| "/run/secrets/git_ssh_key".into());
        setup_ssh(Path::new(&key))?;
    } else {
        info!(%repo_url, "HTTPS/local repo — skipping SSH");
    }

    let mut milestones = vec!["clone_started".into()];
    git_clone(repo_url, git_ref, work_dir)?;
    milestones.push("clone_done".into());

    let (commands, smoke, setup_env) = setup_commands(job);
    if !commands.is_empty() || !smoke.is_empty() {
        milestones.push("deps_install_started".into());
        let mut env_vars: Vec<(String, String)> = std::env::vars().collect();
        for (k, v) in &job.env_public {
            env_vars.push((k.clone(), v.clone()));
        }
        for (k, v) in setup_env {
            env_vars.push((k, v));
        }
        for cmd in commands.iter().chain(smoke.iter()) {
            info!(%cmd, "local provision command");
            let status = Command::new("bash")
                .arg("-lc")
                .arg(cmd)
                .current_dir(work_dir)
                .envs(env_vars.clone())
                .output()
                .with_context(|| format!("spawn command: {cmd}"))?;
            if !status.status.success() {
                bail!(
                    "command failed: {cmd}\n{}",
                    String::from_utf8_lossy(&status.stderr)
                );
            }
        }
        milestones.push("deps_install_done".into());
        if !smoke.is_empty() {
            milestones.push("smoke_ok".into());
        }
    }

    // Always write tools allowlist (may be empty) so fleet-wsexec has a stable contract.
    let tools = crate::policy::tools_from_setup(job.project_setup.as_ref());
    write_tools_file(work_dir, &tools)?;

    milestones.push("ready".into());

    Ok(Provisioned {
        work_dir: work_dir.to_path_buf(),
        repo_url: repo_url.to_string(),
        git_ref: git_ref.to_string(),
        milestones,
        mode: "local".into(),
        detail: Some(json!({
            "tools": tools.keys().cloned().collect::<Vec<_>>(),
        })),
    })
}

/// Write `.fleet-tools.json` + `FLEET_TOOLS.md` into the job work_dir.
/// Always written (even if empty) so `fleet-wsexec` has a stable contract.
pub fn write_tools_file(
    work_dir: &Path,
    tools: &std::collections::BTreeMap<String, crate::job::WorkspaceTool>,
) -> Result<()> {
    for name in tools.keys() {
        crate::policy::resolve_named_tool(tools, name)
            .with_context(|| format!("invalid tool {name}"))?;
    }
    fs::create_dir_all(work_dir).with_context(|| format!("create {}", work_dir.display()))?;
    let tools_path = work_dir.join(".fleet-tools.json");
    let value = serde_json::to_value(tools)?;
    fs::write(&tools_path, serde_json::to_string_pretty(&value)?)
        .with_context(|| format!("write {}", tools_path.display()))?;

    let mut md = String::from("# Fleet workspace tools (allowlisted)\n\nUse only:\n\n");
    for (name, tool) in tools {
        let argv = tool.argv.join(" ");
        md.push_str(&format!("- `fleet-wsexec {name}` -> `{argv}`\n"));
        if let Some(d) = &tool.description {
            if !d.is_empty() {
                md.push_str(&format!("  - {d}\n"));
            }
        }
    }
    if tools.is_empty() {
        md.push_str("(none — no project tools configured)\n");
    }
    md.push_str("\nNo free shell into the workspace.\n");
    fs::write(work_dir.join("FLEET_TOOLS.md"), md)?;
    info!(
        path = %tools_path.display(),
        n = tools.len(),
        "wrote workspace tools allowlist"
    );
    Ok(())
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/fleet"))
}

fn setup_ssh(key_path: &Path) -> Result<()> {
    if !key_path.is_file() {
        bail!(
            "SSH key not found at {} — mount via SSH_KEY_HOST_PATH",
            key_path.display()
        );
    }
    let ssh_dir = home().join(".ssh");
    fs::create_dir_all(&ssh_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ssh_dir, fs::Permissions::from_mode(0o700))?;
    }
    let dest = ssh_dir.join("id_fleet");
    fs::copy(key_path, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
    }
    let config = ssh_dir.join("config");
    let known = ssh_dir.join("known_hosts");
    fs::write(
        &config,
        format!(
            "Host *\n  IdentityFile {}\n  IdentitiesOnly yes\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile {}\n",
            dest.display(),
            known.display()
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    }
    info!(path = %dest.display(), "SSH key installed");
    Ok(())
}

fn git_clone(repo_url: &str, git_ref: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        info!(path = %dest.display(), "removing existing workdir");
        fs::remove_dir_all(dest)?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut env_vars: Vec<(String, String)> = std::env::vars().collect();
    let ssh_cfg = home().join(".ssh").join("config");
    if ssh_cfg.is_file() {
        env_vars.push((
            "GIT_SSH_COMMAND".into(),
            format!("ssh -F {}", ssh_cfg.display()),
        ));
    }

    info!(%repo_url, %git_ref, dest = %dest.display(), "git clone");
    let status = run_git(
        &[
            "clone",
            "--depth",
            "1",
            "--branch",
            git_ref,
            repo_url,
            dest.to_str().unwrap(),
        ],
        &env_vars,
        None,
    );
    if status.is_err() {
        warn!("clone with --branch failed; retry default branch");
        if dest.exists() {
            let _ = fs::remove_dir_all(dest);
        }
        run_git(
            &["clone", "--depth", "1", repo_url, dest.to_str().unwrap()],
            &env_vars,
            None,
        )?;
        if !git_ref.is_empty() {
            run_git(&["checkout", git_ref], &env_vars, Some(dest)).with_context(|| {
                format!("git checkout {git_ref:?} failed after default-branch clone")
            })?;
        }
    }
    info!("clone OK");
    Ok(())
}

fn run_git(args: &[&str], env_vars: &[(String, String)], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let out = cmd.output().context("spawn git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        bail!("git {:?} failed: {} {}", args, err, stdout);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::ProjectSetup;

    #[test]
    fn setup_commands_from_project_setup() {
        let job: Job = serde_json::from_value(serde_json::json!({
            "repo_url": "https://example.com/r.git",
            "project_setup": {
                "commands": ["pip install -r requirements.txt"],
                "smoke_commands": ["python manage.py check"]
            }
        }))
        .unwrap();
        let (c, s, _) = setup_commands(&job);
        assert_eq!(c.len(), 1);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn setup_commands_script_strategy() {
        let mut job: Job = serde_json::from_value(serde_json::json!({
            "repo_url": "https://example.com/r.git"
        }))
        .unwrap();
        job.project_setup = Some(ProjectSetup {
            strategy: "script".into(),
            script: Some("scripts/fleet-setup.sh".into()),
            ..Default::default()
        });
        let (c, _, _) = setup_commands(&job);
        assert!(c[0].contains("scripts/fleet-setup.sh"));
    }

    #[test]
    fn accept_agent_work_dir_keeps_requested_when_ok() {
        let requested = PathBuf::from("/workspace/jobs/j1/repo");
        let got = accept_agent_work_dir(None, &requested).unwrap();
        assert_eq!(got, requested);
        let got =
            accept_agent_work_dir(Some("/workspace/jobs/j1/repo".into()), &requested).unwrap();
        assert_eq!(got, requested);
    }

    #[test]
    fn accept_agent_work_dir_rejects_escape() {
        let requested = PathBuf::from("/workspace/jobs/j1/repo");
        assert!(accept_agent_work_dir(Some("/tmp/evil".into()), &requested).is_err());
        assert!(accept_agent_work_dir(Some("/workspace/jobs/../etc".into()), &requested).is_err());
    }

    #[test]
    fn write_tools_file_always_and_rejects_bad() {
        let dir = std::env::temp_dir().join(format!("fleet-tools-{}", uuid::Uuid::new_v4()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // empty tools still written
        write_tools_file(&dir, &Default::default()).unwrap();
        assert!(dir.join(".fleet-tools.json").is_file());
        assert!(dir.join("FLEET_TOOLS.md").is_file());

        let mut tools = std::collections::BTreeMap::new();
        tools.insert(
            "npm_test".into(),
            crate::job::WorkspaceTool {
                argv: vec!["npm".into(), "test".into()],
                description: Some("unit".into()),
            },
        );
        write_tools_file(&dir, &tools).unwrap();
        let raw = fs::read_to_string(dir.join(".fleet-tools.json")).unwrap();
        assert!(raw.contains("npm_test"));

        tools.insert(
            "evil".into(),
            crate::job::WorkspaceTool {
                argv: vec!["echo".into(), "a;b".into()],
                description: None,
            },
        );
        assert!(write_tools_file(&dir, &tools).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
