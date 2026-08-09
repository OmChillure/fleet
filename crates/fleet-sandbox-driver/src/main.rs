//! In-container sandbox driver: provision env, run harness via ACP, report on bus.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use fleet_sandbox_driver::bus::Bus;
use fleet_sandbox_driver::job::{Job, JobResult};
use fleet_sandbox_driver::{
    acp, handshake, harness, parse_env_bool, policy, provision,
};
use serde_json::json;
use std::time::Instant;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "fleet-sandbox-driver", about = "In-sandbox: provision workspace + drive harness over ACP")]
struct Args {
    /// Redis URL
    #[arg(long, env = "REDIS_URL", default_value = "redis://127.0.0.1:6379/0")]
    redis_url: String,
    #[arg(long, env = "FLEET_JOBS_QUEUE", default_value = "fleet:jobs")]
    jobs_queue: String,
    #[arg(long, env = "FLEET_RESULTS_QUEUE", default_value = "fleet:results")]
    results_queue: String,
    /// Keep consuming jobs (default: one job then exit)
    #[arg(long, env = "FLEET_DRIVER_LOOP", default_value = "false", value_parser = parse_env_bool)]
    driver_loop: bool,
    /// Seed one job from env when queue empty
    #[arg(long, env = "AUTO_SEED_JOB", default_value = "false", value_parser = parse_env_bool)]
    auto_seed_job: bool,
}

fn main() -> Result<()> {
    // Scrub handshake secret before Tokio starts worker threads (edition 2024).
    handshake::init_from_env().context("handshake init")?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?
        .block_on(async_main(args))
}

async fn async_main(args: Args) -> Result<()> {
    info!("fleet-sandbox-driver starting (provision + multi-harness ACP)");

    let mut bus = Bus::connect(&args.redis_url, &args.jobs_queue, &args.results_queue).await?;

    if args.auto_seed_job {
        if let Some(job) = seed_job_from_env() {
            bus.seed_if_empty(&job).await?;
        }
    }

    info!(queue = %args.jobs_queue, "waiting for jobs (BLPOP)");
    loop {
        match bus.blpop_job(5.0).await {
            Ok(Some(job)) => {
                let result = handle_job(job).await;
                if let Err(e) = bus.publish_result(&result).await {
                    error!(error = %e, "publish result failed");
                }
                if !args.driver_loop {
                    info!("FLEET_DRIVER_LOOP not set; exiting after one job");
                    break;
                }
            }
            Ok(None) => {
                // timeout idle
            }
            Err(e) => {
                error!(error = %e, "BLPOP error");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    info!("driver exit");
    Ok(())
}

fn seed_job_from_env() -> Option<Job> {
    let repo = std::env::var("GIT_REPO_URL").ok()?;
    Some(Job {
        job_id: uuid::Uuid::new_v4().to_string(),
        r#type: "analyze_repo".into(),
        harness: std::env::var("HARNESS").unwrap_or_else(|_| "opencode".into()),
        repo_url: Some(repo),
        git_ref: std::env::var("GIT_REF").unwrap_or_else(|_| "main".into()),
        prompt: Some(std::env::var("ANALYZE_PROMPT").unwrap_or_else(|_| {
            "Analyze this repository. Summarize purpose, layout, languages, build/test.".into()
        })),
        model: std::env::var("MODEL")
            .ok()
            .or_else(|| std::env::var("OPENCODE_MODEL").ok())
            .or_else(|| std::env::var("LMSTUDIO_MODEL").ok()),
        timeout_sec: std::env::var("JOB_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600),
        work_dir: None, // default: /workspace/jobs/{job_id}/repo
        auto_approve: false,
        permission_allow: vec![],
        env_public: Default::default(),
        opencode: None,
        opencode_config_path: std::env::var("OPENCODE_CONFIG_PATH").ok(),
        provider: None,
        command: None,
        args: vec![],
        mcp_servers: vec![],
        openai_api_key: None,
        openai_base_url: None,
        xai_api_key: None,
        anthropic_api_key: None,
        lmstudio_base_url: std::env::var("LMSTUDIO_BASE_URL").ok(),
        lmstudio_api_key: std::env::var("LMSTUDIO_API_KEY").ok(),
        project_setup: None,
    })
}

async fn handle_job(job: Job) -> JobResult {
    let started_at = Utc::now();
    let t0 = Instant::now();
    let job_id = job.job_id.clone();
    let job_type = job.r#type.clone();
    info!(%job_id, harness = %job.harness, "=== job ===");

    let mut result = JobResult {
        job_id: job_id.clone(),
        r#type: job_type,
        ok: false,
        started_at: started_at.to_rfc3339(),
        finished_at: String::new(),
        elapsed_sec: 0.0,
        harness: Some(job.harness.clone()),
        harness_command: None,
        repo_url: None,
        git_ref: None,
        work_dir: None,
        analysis: None,
        error: None,
        acp: None,
        opencode: None,
        provider_probe: None,
        provision: None,
    };

    let outcome = async {
        // Provision + provider probe are sync / blocking HTTP — keep them off the
        // Tokio worker (Daniel: spawn_blocking, not std::thread::sleep on runtime).
        let job_sync = job.clone();
        let (harness, job, prov, mut env_map) = tokio::task::spawn_blocking(move || {
            let harness = harness::resolve_harness(&job_sync)?;
            let mut job = job_sync;
            if job.work_dir.is_none() {
                job.work_dir = Some(
                    policy::resolve_work_dir(&job.job_id, None)
                        .display()
                        .to_string(),
                );
            }
            let prov = provision::provision(&job).context("provision")?;
            let env_map = harness::harness_env(&job, &harness.name, &prov.work_dir)?;
            Ok::<_, anyhow::Error>((harness, job, prov, env_map))
        })
        .await
        .context("provision/setup join")??;

        result.harness = Some(harness.name.clone());
        result.harness_command = Some({
            let mut c = vec![harness.command.clone()];
            c.extend(harness.args.iter().cloned());
            c
        });
        info!(harness = %harness.name, command = ?result.harness_command, "resolved harness");

        result.repo_url = Some(prov.repo_url.clone());
        result.git_ref = Some(prov.git_ref.clone());
        result.work_dir = Some(prov.work_dir.display().to_string());
        result.provision = Some(json!({
            "mode": prov.mode,
            "milestones": prov.milestones,
            "detail": prov.detail,
            "job_work_root": policy::job_root_for_work_dir(&prov.work_dir)
                .display()
                .to_string(),
        }));
        if let Some(probe) = env_map.remove("_FLEET_PROVIDER_PROBE") {
            if let Ok(v) = serde_json::from_str(&probe) {
                result.provider_probe = Some(v);
            }
        }
        env_map.insert("FLEET_JOB_ID".into(), job.job_id.clone());
        env_map.insert(
            "FLEET_WORK_DIR".into(),
            prov.work_dir.display().to_string(),
        );

        let model = job
            .model
            .clone()
            .or_else(|| std::env::var("MODEL").ok())
            .or_else(|| std::env::var("OPENCODE_MODEL").ok())
            .or_else(|| std::env::var("LMSTUDIO_MODEL").ok());

        let mut prompt = job
            .prompt
            .clone()
            .or_else(|| std::env::var("ANALYZE_PROMPT").ok())
            .unwrap_or_else(|| "Analyze this repository briefly.".into());

        // Inject project tool catalogue so the agent uses named workspace tools only.
        if let Some(ps) = &job.project_setup {
            if !ps.tools.is_empty() {
                let mut block = String::from(
                    "\n\n## Workspace tools (allowlisted)\n\
                     Code is on the shared volume (this cwd). To run project checks/tests, \
                     use only these commands (they run in the workspace container, not the harness):\n",
                );
                for (name, tool) in &ps.tools {
                    let desc = tool.description.as_deref().unwrap_or("");
                    let argv = tool.argv.join(" ");
                    block.push_str(&format!(
                        "- `fleet-wsexec {name}` → `{argv}` {desc}\n"
                    ));
                }
                block.push_str(
                    "Do **not** invent arbitrary shell for installs/tests. \
                     Prefer fleet-wsexec <tool-name>.\n",
                );
                prompt.push_str(&block);
            }
        }

        info!(
            auto_approve = job.auto_approve,
            allow = ?job.permission_allow,
            raw_exec = policy::raw_exec_from_env(),
            "permission policy"
        );

        let acp = acp::run_session(
            &harness,
            &prov.work_dir,
            model.as_deref(),
            &prompt,
            env_map,
            job.auto_approve,
            &job.permission_allow,
            job.timeout_sec,
            &job.mcp_servers,
        )
        .await?;

        let transcript = acp.transcript.trim().to_string();
        // Keep a generous transcript on the result so lab scripts can print what the model did.
        let stdout_keep: String = transcript.chars().rev().take(48_000).collect::<String>().chars().rev().collect();
        let stderr_keep: String = acp
            .stderr_tail
            .chars()
            .rev()
            .take(8_000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let acp_json = json!({
            "ok": acp.ok,
            "protocol": "acp",
            "harness": harness.name,
            "model": acp.model,
            "session_id": acp.session_id,
            "stop_reason": acp.stop_reason,
            "elapsed_sec": acp.elapsed_sec,
            "command": acp.command,
            "agent_info": acp.agent_info,
            "transcript_chars": transcript.chars().count(),
            "stdout_tail": stdout_keep,
            "stderr_tail": stderr_keep,
        });
        result.acp = Some(acp_json.clone());
        result.opencode = Some(acp_json);
        result.analysis = Some(transcript);
        if !acp.ok {
            result.error = acp.error.or_else(|| {
                if acp.stderr_tail.is_empty() {
                    None
                } else {
                    Some(acp.stderr_tail.chars().rev().take(2000).collect::<String>().chars().rev().collect())
                }
            });
        }
        // ok if session completed cleanly; empty transcript still ok when stop_reason is end_turn
        // (tool-only turns may produce little text but tool_call lines still fill analysis when present)
        result.ok = acp.ok
            && (result.analysis.as_ref().is_some_and(|a| !a.is_empty())
                || acp
                    .stop_reason
                    .as_ref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("end_turn")));
        if let Some(ref a) = result.analysis {
            info!(
                model = ?acp.model,
                chars = a.chars().count(),
                "ACP done"
            );
            tracing::debug!(
                preview = %a.chars().take(200).collect::<String>(),
                "model transcript preview"
            );
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = outcome {
        error!(error = %e, "job failed");
        result.ok = false;
        result.error = Some(format!("{e:#}"));
    }

    result.elapsed_sec = t0.elapsed().as_secs_f64();
    result.finished_at = Utc::now().to_rfc3339();
    result
}
