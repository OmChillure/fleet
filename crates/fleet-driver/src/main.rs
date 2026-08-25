//! Process-backend driver: ACP session + orchestrator worker protocol.
//!
//! Same `FLEET_*` contract as `fleet-worker`. Default harness is `mock`
//! (`mock-acp-agent`). Set `FLEET_HARNESS=grok` to drive Grok Build.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use anyhow::Result;
use fleet_driver::acp::{self, AcpRun, is_blocked_stop};
use fleet_driver::config::DriverConfig;
use fleet_driver::harness;
use fleet_driver::orch::{OrchClient, truncate_reason};
use fleet_driver::prompt;
use fleet_driver::provision::{self, RepoToClone};
use fleet_driver::report;
use serde_json::json;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    if let Err(e) = run().await {
        eprintln!("fleet-driver fatal: {e:#}");
        std::process::exit(2);
    }
}

async fn run() -> Result<()> {
    let cfg = DriverConfig::from_env()?;
    info!(
        session = %cfg.session_id,
        project = %cfg.project_id,
        task = %cfg.board_task_id,
        harness = %cfg.harness,
        work_dir = %cfg.work_dir.display(),
        "fleet-driver start"
    );

    let client = OrchClient::new(&cfg.api_base, &cfg.project_id, &cfg.session_id, &cfg.token)?;

    client.post_status(1, "running", None).await?;
    client.post_heartbeat(2, Some(5)).await?;

    let stop = Arc::new(AtomicBool::new(false));
    let seq = Arc::new(AtomicI32::new(3));
    let hb_stop = stop.clone();
    let hb_seq = seq.clone();
    let hb_client = client.clone();
    let hb_interval = cfg.heartbeat_interval;
    let hb = tokio::spawn(async move {
        let mut pct = 10i32;
        loop {
            tokio::time::sleep(hb_interval).await;
            if hb_stop.load(Ordering::Relaxed) {
                break;
            }
            pct = (pct + 15).min(90);
            let s = hb_seq.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = hb_client.post_heartbeat(s, Some(pct)).await {
                warn!(error = %e, "heartbeat failed");
            }
        }
    });

    let work = execute_agent(&cfg).await;
    stop.store(true, Ordering::Relaxed);
    let _ = tokio::time::timeout(Duration::from_secs(2), hb).await;

    match work {
        Ok(summary) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let _ = client.post_heartbeat(s, Some(100)).await;
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let proof = std::fs::read_to_string(cfg.work_dir.join("result.md")).ok();
            let files = guest_files(&cfg.work_dir);
            client
                .post_status_proof(s, "success", Some(&summary), proof.as_deref(), files)
                .await?;
            info!(%summary, "fleet-driver success");
            Ok(())
        }
        Err(WorkError::Blocked(reason)) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let r = truncate_reason(&reason, 512);
            client.post_status(s, "blocked", Some(&r)).await?;
            info!(%reason, "fleet-driver blocked");
            Ok(())
        }
        Err(WorkError::Failed(reason)) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let r = truncate_reason(&reason, 512);
            let _ = client.post_status(s, "failed", Some(&r)).await;
            error!(%reason, "fleet-driver failed");
            anyhow::bail!("{reason}")
        }
    }
}

fn guest_files(work_dir: &std::path::Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut files = serde_json::Map::new();
    if work_dir.join("work").is_dir() {
        files.insert("work/.keep".into(), serde_json::Value::String("ok".into()));
    }
    if files.is_empty() {
        None
    } else {
        Some(files)
    }
}

enum WorkError {
    Failed(String),
    Blocked(String),
}

async fn provision_if_needed(cfg: &DriverConfig) -> Result<std::path::PathBuf, WorkError> {
    let spec = cfg
        .repo_url
        .as_ref()
        .map(|url| RepoToClone {
            url: url.clone(),
            git_ref: cfg.git_ref.clone(),
        })
        .or_else(|| RepoToClone::from_env_and_files(&cfg.work_dir));
    let Some(spec) = spec else {
        return Ok(cfg.work_dir.clone());
    };
    let timeout = provision::clone_timeout(cfg.timeout);
    match tokio::time::timeout(timeout, provision::ensure_repo(&cfg.work_dir, &spec)).await {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(e)) => Err(WorkError::Failed(format!("provision clone: {e:#}"))),
        Err(_) => Err(WorkError::Failed(format!(
            "provision clone timed out after {}s",
            timeout.as_secs()
        ))),
    }
}

async fn execute_agent(cfg: &DriverConfig) -> Result<String, WorkError> {
    let agent_cwd = provision_if_needed(cfg).await?;
    let prompt = prompt::load_prompt(&cfg.work_dir);
    let mut spec = harness::resolve(
        &cfg.harness,
        cfg.harness_cmd.as_deref(),
        cfg.model.as_deref(),
        cfg.auto_approve,
        cfg.mock_bin.as_deref(),
        cfg.grok_bin.as_deref(),
        cfg.reasoning_effort.as_deref(),
    )
    .map_err(|e| WorkError::Failed(format!("harness: {e:#}")))?;
    if spec.name == "grok" && cfg.grok_debug {
        let dbg = cfg.work_dir.join("grok-acp.debug.log");
        // `grok agent [OPTIONS] stdio` — insert before the subcommand.
        let insert_at = spec.args.len().saturating_sub(1);
        spec.args.splice(
            insert_at..insert_at,
            ["--debug-file".into(), dbg.display().to_string()],
        );
    }

    let mut extra_env = HashMap::new();
    extra_env.insert("FLEET_SESSION_ID".into(), cfg.session_id.clone());
    extra_env.insert("FLEET_PROJECT_ID".into(), cfg.project_id.clone());

    let outcome = acp::run_session(AcpRun {
        harness: spec.clone(),
        cwd: agent_cwd.clone(),
        session_dir: cfg.work_dir.clone(),
        prompt: prompt.clone(),
        model: cfg.model.clone(),
        auto_approve: cfg.auto_approve,
        timeout: cfg.timeout,
        extra_env,
    })
    .await
    .map_err(|e| WorkError::Failed(format!("acp: {e:#}")))?;

    let result = json!({
        "worker": "fleet-driver",
        "ok": outcome.ok,
        "harness": spec.name,
        "command": outcome.command,
        "model": outcome.model,
        "session_id": cfg.session_id,
        "acp_session_id": outcome.session_id,
        "stop_reason": outcome.stop_reason,
        "elapsed_sec": outcome.elapsed_sec,
        "agent_info": outcome.agent_info,
        "error": outcome.error,
        "usage": outcome.usage,
        "transcript_chars": outcome.transcript.chars().count(),
        "plan_task_id": cfg.plan_task_id,
        "board_task_id": cfg.board_task_id,
        "agent_cwd": agent_cwd.display().to_string(),
        "repo_url": cfg.repo_url,
        "git_ref": cfg.git_ref,
    });
    if let Err(e) = report::write_artifacts(&cfg.work_dir, &result, &outcome.transcript) {
        warn!(error = %e, "failed to write artifacts");
    }

    let blocked = outcome
        .stop_reason
        .as_deref()
        .is_some_and(is_blocked_stop);
    if outcome.ok && !blocked {
        Ok(format!(
            "acp ok harness={} stop={} elapsed={:.2}s chars={}",
            spec.name,
            outcome.stop_reason.as_deref().unwrap_or("-"),
            outcome.elapsed_sec,
            outcome.transcript.chars().count()
        ))
    } else {
        let mut reason = outcome.error.unwrap_or_else(|| "ACP session failed".into());
        if !outcome.stderr_tail.is_empty() {
            let tail: String = outcome
                .stderr_tail
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            reason.push_str(" — stderr: ");
            reason.push_str(&tail);
        }
        if blocked {
            Err(WorkError::Blocked(reason))
        } else {
            Err(WorkError::Failed(reason))
        }
    }
}
