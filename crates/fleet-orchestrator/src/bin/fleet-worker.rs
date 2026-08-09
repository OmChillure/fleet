//! Real process-backend worker (PR7).
//!
//! Implements the control-plane worker protocol:
//!   POST /v1/projects/{project}/workers/{session}/heartbeat
//!   POST /v1/projects/{project}/workers/{session}/status
//!
//! Then executes real work (not a sleep stub):
//!   1. `FLEET_WORK_CMD` if set (shell command)
//!   2. else `$FLEET_WORK_DIR/run.sh` if present
//!   3. else `$FLEET_WORK_DIR/spec.md` / plan notes → write artifact under work dir
//!
//! Env (injected by orchestrator spawn):
//!   FLEET_PROJECT_ID, FLEET_SESSION_ID, FLEET_RUN_ID, FLEET_BOARD_TASK_ID / FLEET_TASK_ID,
//!   FLEET_WORKER_TOKEN, FLEET_ORCHESTRATOR_URL | FLEET_API_BASE, FLEET_WORK_DIR,
//!   FLEET_PLAN_TASK_ID, FLEET_HITL_NOTES (optional), FLEET_WORK_CMD (optional),
//!   FLEET_HEARTBEAT_INTERVAL_S (default 2)

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

fn main() {
    if let Err(e) = run() {
        eprintln!("fleet-worker fatal: {e:#}");
        std::process::exit(2);
    }
}

fn run() -> anyhow::Result<()> {
    let cfg = WorkerConfig::from_env()?;
    eprintln!(
        "fleet-worker start session={} project={} task={} api={} work_dir={}",
        cfg.session_id,
        cfg.project_id,
        cfg.board_task_id,
        cfg.api_base,
        cfg.work_dir.display()
    );

    let client = Client::new(cfg.clone());

    // Announce running on the control plane.
    client.post_status(1, "running", None)?;
    client.post_heartbeat(2, Some(5))?;

    let stop = Arc::new(AtomicBool::new(false));
    let seq = Arc::new(AtomicI32::new(3));
    let hb_stop = stop.clone();
    let hb_seq = seq.clone();
    let hb_client = client.clone();
    let hb_interval = cfg.heartbeat_interval;
    let hb = thread::spawn(move || {
        let mut pct = 10i32;
        while !hb_stop.load(Ordering::Relaxed) {
            thread::sleep(hb_interval);
            if hb_stop.load(Ordering::Relaxed) {
                break;
            }
            pct = (pct + 15).min(90);
            let s = hb_seq.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = hb_client.post_heartbeat(s, Some(pct)) {
                eprintln!("fleet-worker heartbeat failed: {e:#}");
            }
        }
    });

    let work_result = execute_work(&cfg);
    stop.store(true, Ordering::Relaxed);
    let _ = hb.join();

    match work_result {
        Ok(summary) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            client.post_heartbeat(s, Some(100))?;
            let s = seq.fetch_add(1, Ordering::Relaxed);
            client.post_status(s, "success", Some(&summary))?;
            eprintln!("fleet-worker success: {summary}");
            Ok(())
        }
        Err(WorkError::Blocked(reason)) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            client.post_status(s, "blocked", Some(&reason))?;
            eprintln!("fleet-worker blocked: {reason}");
            // Exit 0 so process backend does not double-fail the task; inbox owns blocked.
            Ok(())
        }
        Err(WorkError::Failed(reason)) => {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let _ = client.post_status(s, "failed", Some(&reason));
            eprintln!("fleet-worker failed: {reason}");
            anyhow::bail!("{reason}")
        }
    }
}

#[derive(Clone)]
struct WorkerConfig {
    api_base: String,
    project_id: String,
    session_id: String,
    board_task_id: String,
    plan_task_id: String,
    token: String,
    work_dir: PathBuf,
    work_cmd: Option<String>,
    hitl_notes: Option<String>,
    heartbeat_interval: Duration,
}

impl WorkerConfig {
    fn from_env() -> anyhow::Result<Self> {
        let require = |k: &str| {
            env::var(k).map_err(|_| anyhow::anyhow!("missing required env {k}"))
        };
        let api_base = env::var("FLEET_ORCHESTRATOR_URL")
            .or_else(|_| env::var("FLEET_API_BASE"))
            .map_err(|_| anyhow::anyhow!("missing FLEET_ORCHESTRATOR_URL or FLEET_API_BASE"))?
            .trim_end_matches('/')
            .to_string();
        let work_dir = PathBuf::from(require("FLEET_WORK_DIR")?);
        let board_task_id = env::var("FLEET_BOARD_TASK_ID")
            .or_else(|_| env::var("FLEET_TASK_ID"))
            .map_err(|_| anyhow::anyhow!("missing FLEET_BOARD_TASK_ID / FLEET_TASK_ID"))?;
        let hb_s: u64 = env::var("FLEET_HEARTBEAT_INTERVAL_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2)
            .max(1);
        Ok(Self {
            api_base,
            project_id: require("FLEET_PROJECT_ID")?,
            session_id: require("FLEET_SESSION_ID")?,
            board_task_id,
            plan_task_id: env::var("FLEET_PLAN_TASK_ID").unwrap_or_default(),
            token: require("FLEET_WORKER_TOKEN")?,
            work_dir,
            work_cmd: env::var("FLEET_WORK_CMD").ok().filter(|s| !s.trim().is_empty()),
            hitl_notes: env::var("FLEET_HITL_NOTES").ok().filter(|s| !s.trim().is_empty()),
            heartbeat_interval: Duration::from_secs(hb_s),
        })
    }
}

#[derive(Clone)]
struct Client {
    cfg: WorkerConfig,
    http: reqwest::blocking::Client,
}

impl Client {
    fn new(cfg: WorkerConfig) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        Self { cfg, http }
    }

    fn worker_url(&self, tail: &str) -> String {
        format!(
            "{}/v1/projects/{}/workers/{}/{}",
            self.cfg.api_base, self.cfg.project_id, self.cfg.session_id, tail
        )
    }

    fn post_heartbeat(&self, seq: i32, progress_pct: Option<i32>) -> anyhow::Result<()> {
        let url = self.worker_url("heartbeat");
        let body = json!({ "seq": seq, "progress_pct": progress_pct });
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.token)
            .json(&body)
            .send()?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().unwrap_or_default();
            anyhow::bail!("heartbeat {status}: {text}");
        }
        Ok(())
    }

    fn post_status(&self, seq: i32, status: &str, reason: Option<&str>) -> anyhow::Result<()> {
        let url = self.worker_url("status");
        let body = json!({
            "seq": seq,
            "status": status,
            "reason": reason,
        });
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.token)
            .json(&body)
            .send()?;
        if !res.status().is_success() {
            let code = res.status();
            let text = res.text().unwrap_or_default();
            anyhow::bail!("status {code}: {text}");
        }
        Ok(())
    }
}

enum WorkError {
    Failed(String),
    Blocked(String),
}

fn execute_work(cfg: &WorkerConfig) -> Result<String, WorkError> {
    // HITL re-entry: notes available for the worker to apply.
    if let Some(notes) = &cfg.hitl_notes {
        let notes_path = cfg.work_dir.join("hitl_notes.txt");
        let _ = std::fs::write(&notes_path, notes);
    }

    // Force blocked path for fixtures/tests.
    if env::var("FLEET_FORCE_BLOCKED").ok().as_deref() == Some("1") {
        return Err(WorkError::Blocked(
            env::var("FLEET_BLOCK_REASON").unwrap_or_else(|_| "worker requested HITL".into()),
        ));
    }
    if env::var("FLEET_FORCE_FAIL").ok().as_deref() == Some("1") {
        return Err(WorkError::Failed(
            env::var("FLEET_FAIL_REASON").unwrap_or_else(|_| "worker forced failure".into()),
        ));
    }

    if let Some(cmd) = &cfg.work_cmd {
        return run_shell(cfg, cmd);
    }

    let run_sh = cfg.work_dir.join("run.sh");
    if run_sh.is_file() {
        // Ensure executable bit is not required — run via sh.
        return run_shell(cfg, &format!("sh {}", run_sh.display()));
    }

    // Default real work: materialize an artifact from the plan task + optional spec.
    default_artifact_work(cfg)
}

fn run_shell(cfg: &WorkerConfig, cmd: &str) -> Result<String, WorkError> {
    let start = Instant::now();
    let log_out = cfg.work_dir.join("worker_cmd.stdout");
    let log_err = cfg.work_dir.join("worker_cmd.stderr");
    let out_file = std::fs::File::create(&log_out)
        .map_err(|e| WorkError::Failed(format!("stdout log: {e}")))?;
    let err_file = std::fs::File::create(&log_err)
        .map_err(|e| WorkError::Failed(format!("stderr log: {e}")))?;

    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(&cfg.work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .env("FLEET_PROJECT_ID", &cfg.project_id)
        .env("FLEET_SESSION_ID", &cfg.session_id)
        .env("FLEET_BOARD_TASK_ID", &cfg.board_task_id)
        .env("FLEET_PLAN_TASK_ID", &cfg.plan_task_id)
        .status()
        .map_err(|e| WorkError::Failed(format!("spawn work cmd: {e}")))?;

    let elapsed = start.elapsed();
    if status.success() {
        Ok(format!(
            "cmd ok in {:.2}s: {cmd}",
            elapsed.as_secs_f64()
        ))
    } else {
        let stderr = std::fs::read_to_string(&log_err).unwrap_or_default();
        let snippet: String = stderr.chars().take(500).collect();
        Err(WorkError::Failed(format!(
            "cmd exit {:?}: {cmd} — {snippet}",
            status.code()
        )))
    }
}

/// Default path when no run.sh / FLEET_WORK_CMD: produce a durable artifact in the session work dir.
fn default_artifact_work(cfg: &WorkerConfig) -> Result<String, WorkError> {
    std::fs::create_dir_all(&cfg.work_dir)
        .map_err(|e| WorkError::Failed(format!("work_dir: {e}")))?;

    let spec = read_optional(cfg.work_dir.join("spec.md"));
    let artifact_dir = cfg.work_dir.join("artifacts");
    std::fs::create_dir_all(&artifact_dir)
        .map_err(|e| WorkError::Failed(format!("artifacts dir: {e}")))?;

    let out = artifact_dir.join("result.json");
    let body = json!({
        "worker": "fleet-worker",
        "plan_task_id": cfg.plan_task_id,
        "board_task_id": cfg.board_task_id,
        "session_id": cfg.session_id,
        "project_id": cfg.project_id,
        "hitl_notes": cfg.hitl_notes,
        "spec_md_present": spec.is_some(),
        "spec_md_bytes": spec.as_ref().map(|s| s.len()).unwrap_or(0),
        "completed_at": chrono_lite_now(),
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&body).unwrap())
        .map_err(|e| WorkError::Failed(format!("write artifact: {e}")))?;

    // Also emit a short human-readable summary.
    let summary_path = artifact_dir.join("SUMMARY.md");
    let summary = format!(
        "# Task complete\n\n- plan_task_id: `{}`\n- board_task_id: `{}`\n- artifact: `{}`\n",
        cfg.plan_task_id,
        cfg.board_task_id,
        out.display()
    );
    std::fs::write(&summary_path, &summary)
        .map_err(|e| WorkError::Failed(format!("write summary: {e}")))?;

    Ok(format!(
        "wrote artifacts for plan_task_id={} → {}",
        cfg.plan_task_id,
        out.display()
    ))
}

fn read_optional(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn chrono_lite_now() -> String {
    // Avoid pulling chrono into the binary if not needed — use system time RFC3339-ish.
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}
