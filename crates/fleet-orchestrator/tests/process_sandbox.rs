//! Process sandbox + `fleet-driver` e2e.
//!
//! Spawns the guest binary that heartbeats/status via `/v1` and writes
//! artifacts under the session work dir. Requires `DATABASE_URL`. Unix only.

#![cfg(unix)]

use fleet_orchestrator::api::{build_app_state, router};
use fleet_orchestrator::config::{AuthMode, Config};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::sandbox::{ProcessRuntime, SandboxRuntime};
use fleet_orchestrator::store::{Store, connect, migrate};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn require_database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "DATABASE_URL is required for this integration test. \
                 Start Postgres (just db-up) and export DATABASE_URL."
            )
        })
}

fn driver_bin() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("target")
        });
    let driver = target.join(profile).join("fleet-driver");
    {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut cmd = std::process::Command::new(cargo);
        cmd.args(["build", "-p", "fleet-driver", "--bins", "--quiet"]);
        if !cfg!(debug_assertions) {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("cargo build -p fleet-driver");
        assert!(status.success(), "failed to build fleet-driver");
    }
    assert!(
        driver.is_file(),
        "fleet-driver binary missing at {}",
        driver.display()
    );
    driver
}

#[tokio::test]
async fn e2e_process_worker_completes_chain_and_writes_artifacts() {
    let database_url = require_database_url();
    let driver = driver_bin();

    let data_dir = tempfile::tempdir().expect("tempdir");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");

    let cfg = Config {
        database_url,
        admin_token: "test-admin".into(),
        auth_mode: AuthMode::Token,
        http_addr: addr.to_string(),
        orchestrator_public_url: format!("http://{addr}"),
        data_dir: data_dir.path().to_path_buf(),
        tick_interval: Duration::from_secs(5),
        max_agents: 3,
        heartbeat_stale: Duration::from_secs(60),
        spawn_timeout: Duration::from_secs(30),
        kill_grace: Duration::from_secs(1),
        default_task_timeout: Duration::from_secs(3600),
        default_max_retries: 2,
        retry_backoff: Duration::from_secs(1),
        on_task_fail: "fail_run".into(),
        auto_sync_external: false,
        sandbox_backend: "process".into(),
        stub_duration: Duration::from_millis(0),
        process_worker_argv: vec![driver.to_string_lossy().into_owned()],
        k8s: fleet_orchestrator::sandbox::K8sOpts::default(),
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    };

    // Repo-root .env sets these to Kind :8080. Spawn must use
    // cfg.orchestrator_public_url, not public_orchestrator_url() at tick time.
    unsafe {
        std::env::set_var("API_PUBLIC_BASE", "http://127.0.0.1:1");
        std::env::set_var("ORCHESTRATOR_PUBLIC_URL", "http://127.0.0.1:1");
    }

    let state = build_app_state(&cfg).await.expect("state");
    state.tick.enable_local_spawn();
    assert_eq!(state.tick.runtime_backend(), "process");
    let store = state.store.clone();
    let tick = state.tick.clone();

    tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    });
    // let the listener accept
    tokio::time::sleep(Duration::from_millis(50)).await;

    let project_id = Uuid::new_v4();
    store
        .ensure_project(project_id, "process-e2e", 3)
        .await
        .expect("project");

    let plan = WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "process-e2e".into(),
        tasks: vec![
            PlanTask {
                plan_task_id: "t1".into(),
                title: "Root".into(),
                summary: "first".into(),
                spec_md: "# t1\nwrite artifact\n".into(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 100,
                external_ref: None,
                timeout_s: Some(60),
                max_retries: Some(1),
            },
            PlanTask {
                plan_task_id: "t2".into(),
                title: "Child".into(),
                summary: "second".into(),
                spec_md: "# t2\nfinish chain\n".into(),
                depends_on: vec!["t1".into()],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 50,
                external_ref: None,
                timeout_s: Some(60),
                max_retries: Some(1),
            },
        ],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    };
    let run_id = plan.run_id;
    store
        .insert_accepted_plan(project_id, plan)
        .await
        .expect("plan");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last_status = String::new();
    while tokio::time::Instant::now() < deadline {
        let report = tick.tick_project(project_id).await.expect("tick");
        last_status = sqlx::query_scalar::<_, String>("SELECT status FROM work_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(store.pool())
            .await
            .expect("run status");
        if last_status == "completed" {
            let _ = report;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if last_status != "completed" {
        let tasks: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT plan_task_id, status, failure_reason FROM board_tasks WHERE run_id = $1 ORDER BY plan_task_id",
        )
        .bind(run_id)
        .fetch_all(store.pool())
        .await
        .unwrap_or_default();
        panic!(
            "run did not complete via process worker: status={last_status} tasks={tasks:?} \
             (workers must call this test's listener, not API_PUBLIC_BASE / Kind :8080)"
        );
    }

    let tasks: Vec<(String, String)> = sqlx::query_as(
        "SELECT plan_task_id, status FROM board_tasks WHERE run_id = $1 ORDER BY plan_task_id",
    )
    .bind(run_id)
    .fetch_all(store.pool())
    .await
    .expect("tasks");
    assert_eq!(
        tasks,
        vec![
            ("t1".into(), "done_success".into()),
            ("t2".into(), "done_success".into())
        ]
    );

    let sessions = store
        .list_sessions_for_project(project_id)
        .await
        .expect("sessions");
    assert!(
        sessions.iter().any(|s| s.runtime_backend == "process"),
        "expected a process session, got {:?}",
        sessions
            .iter()
            .map(|s| &s.runtime_backend)
            .collect::<Vec<_>>()
    );

    let mut found_artifact = false;
    let mut diag = Vec::new();
    for s in &sessions {
        let Some(dir) = s.work_dir.as_ref() else {
            diag.push(format!(
                "session {} has no work_dir status={}",
                s.id, s.status
            ));
            continue;
        };
        let root = PathBuf::from(dir);
        let stderr = std::fs::read_to_string(root.join("stderr.log")).unwrap_or_default();
        let stdout = std::fs::read_to_string(root.join("stdout.log")).unwrap_or_default();
        diag.push(format!(
            "session {} status={} backend={} dir={} stderr={:?} stdout={:?}",
            s.id, s.status, s.runtime_backend, dir, stderr, stdout
        ));
        let artifact = root.join("artifacts").join("result.json");
        if artifact.is_file() {
            let body: Value =
                serde_json::from_str(&std::fs::read_to_string(&artifact).unwrap()).unwrap();
            assert_eq!(body["worker"], "fleet-driver");
            assert_eq!(body["harness"], "mock");
            found_artifact = true;
        }
        let task_json = root.join("task.json");
        assert!(
            task_json.is_file(),
            "missing task.json in {}\n{}",
            root.display(),
            diag.join("\n")
        );
        let tj: Value =
            serde_json::from_str(&std::fs::read_to_string(&task_json).unwrap()).unwrap();
        assert!(!tj["title"].as_str().unwrap_or("").is_empty());
        assert!(tj["timeout_s"].as_u64().unwrap_or(0) >= 1);
        let prompt = std::fs::read_to_string(root.join("prompt.md")).unwrap_or_default();
        assert!(
            prompt.contains('#') || !prompt.is_empty(),
            "prompt.md empty in {}",
            root.display()
        );
    }
    assert!(
        found_artifact,
        "fleet-driver did not write artifacts/result.json\n{}",
        diag.join("\n")
    );
}

/// Docs §16: `SANDBOX_BACKEND=process` without a worker bin → spawn fails →
/// session `failed_spawn` → task not left assigned → run fails (`fail_run`).
/// Must not complete via a silent stub.
#[tokio::test]
async fn process_backend_missing_worker_bin_is_failed_spawn_then_fail_run() {
    let url = require_database_url();
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("migrate");
    let store = Store::new(pool);

    let data_dir = tempfile::tempdir().expect("tempdir");
    let missing = data_dir.path().join("no-such-fleet-driver");
    assert!(
        !missing.exists(),
        "fixture must not exist: {}",
        missing.display()
    );
    let rt = Arc::new(
        ProcessRuntime::new(
            data_dir.path(),
            vec![missing.to_string_lossy().into_owned()],
        )
        .expect("ProcessRuntime accepts a missing bin path; exec fails at spawn"),
    );
    assert_eq!(rt.backend_name(), "process");
    assert_ne!(rt.backend_name(), "stub");

    let runner = TickRunner::new(
        store.clone(),
        rt,
        data_dir.path().to_path_buf(),
        "http://127.0.0.1:1",
    )
    .with_local_spawn();

    let project = store
        .create_project(&format!("missing-bin-{}", Uuid::new_v4()), 3)
        .await
        .expect("project");
    let cfg = store.get_project_config(project.id).await.expect("config");
    assert_eq!(
        cfg.on_task_fail, "fail_run",
        "project default must be fail_run (docs §16); got {}",
        cfg.on_task_fail
    );

    let plan = WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id: project.id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "missing-bin".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: "Only".into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(60),
            max_retries: Some(0),
        }],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    };
    let run_id = plan.run_id;
    store
        .insert_accepted_plan(project.id, plan)
        .await
        .expect("plan");

    let r1 = runner.tick_project(project.id).await.expect("tick1");
    // Same tick materializes, promotes t1, spawn-fails, rollup. Extra tick is
    // a no-op if already terminal.
    let r2 = runner.tick_project(project.id).await.expect("tick2");

    assert_eq!(
        r1.spawned + r2.spawned,
        0,
        "missing worker bin must not succeed spawn (no silent stub): r1={r1:?} r2={r2:?}"
    );
    assert!(
        r1.spawn_failed + r2.spawn_failed >= 1,
        "missing worker bin must count spawn_failed: r1={r1:?} r2={r2:?}"
    );
    assert_eq!(
        r1.completed_via_runtime + r2.completed_via_runtime,
        0,
        "must not complete via stub/runtime success: r1={r1:?} r2={r2:?}"
    );

    let snap = store.load_board_snapshot(project.id).await.expect("snap");
    assert!(
        !snap.sessions.is_empty(),
        "spawn attempt must leave a session row (failed_spawn), not skip assignment"
    );
    for s in &snap.sessions {
        assert_eq!(
            s.runtime_backend, "process",
            "must not fall back to stub or another backend: {s:?}"
        );
        assert_ne!(s.runtime_backend, "stub");
        assert_eq!(
            s.status, "failed_spawn",
            "session must be failed_spawn, not left spawning/healthy: {s:?}"
        );
        assert_eq!(
            s.exit_reason.as_deref(),
            Some("spawn_failed"),
            "exit_reason: {s:?}"
        );
        assert!(
            s.runtime_handle.is_none(),
            "failed exec must not record a runtime handle: {s:?}"
        );
        if let Some(dir) = s.work_dir.as_ref() {
            assert!(
                !PathBuf::from(dir).join("pid").exists(),
                "missing bin must not write a pidfile in {dir}"
            );
        }
    }

    let t1 = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .expect("t1");
    assert_eq!(
        t1.status, "done_failed",
        "task must not stay assigned/in_progress/ready after failed_spawn: {t1:?}"
    );
    assert!(
        t1.claimed_session_id.is_none(),
        "failed_spawn must release the claim: {t1:?}"
    );
    assert_eq!(
        t1.failure_reason.as_deref(),
        Some("spawn_failed"),
        "failure_reason: {t1:?}"
    );

    let run_status: String = sqlx::query_scalar("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .expect("run status");
    assert_eq!(
        run_status, "failed",
        "fail_run must terminate the run as failed after spawn_failed; \
         r1={r1:?} r2={r2:?}"
    );
}
