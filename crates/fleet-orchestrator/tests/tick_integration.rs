//! PR5 tick integration tests (materialize, deps, spawn stub, cancel, rollup).

use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::{TickError, TickRunner};
use fleet_orchestrator::sandbox::StubRuntime;
use fleet_orchestrator::store::{NewWorkerSession, Store, StoreError, connect, migrate};
use std::path::{Path, PathBuf};
use std::process::Command;
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

async fn setup() -> Store {
    let url = require_database_url();
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("migrate");
    Store::new(pool)
}

fn runner(store: Store, stub_ms: u64) -> TickRunner {
    let rt = Arc::new(StubRuntime::new(Duration::from_millis(stub_ms)));
    TickRunner::with_store_and_runtime(store, rt)
}

fn chain_plan(project_id: Uuid) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![
            PlanTask {
                plan_task_id: "t1".into(),
                title: "Root".into(),
                summary: String::new(),
                spec_md: String::new(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 100,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: Some(1),
            },
            PlanTask {
                plan_task_id: "t2".into(),
                title: "Child".into(),
                summary: String::new(),
                spec_md: String::new(),
                depends_on: vec!["t1".into()],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 50,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: None,
            },
        ],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn tick_materializes_and_promotes_root_to_ready() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    store
        .insert_accepted_plan(project.id, chain_plan(project.id))
        .await
        .unwrap();

    // Long stub duration so we don't complete before assertions
    let r = runner(store.clone(), 60_000)
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 1);
    assert_eq!(r.tasks_materialized, 2);
    assert_eq!(r.dep_updates, 1);
    assert_eq!(r.spawned, 1); // root only

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "in_progress");
    assert_eq!(t2.status, "queued");
    assert_eq!(snap.sessions.len(), 1);
}

#[tokio::test]
async fn tick_stub_completes_chain_end_to_end() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-e2e-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    // Duration 0: process_sessions treats stub as exited in the same tick after spawn.
    // Tick N deps run before spawn, so a two-task chain needs two ticks minimum:
    //   tick1: t1 ready→spawn→done; tick2: t2 ready→spawn→done→run completed.
    let runner = runner(store.clone(), 0);
    let r1 = runner.tick_project(project.id).await.unwrap();
    assert!(r1.spawned >= 1, "r1={r1:?}");
    assert!(
        r1.completed_via_runtime >= 1,
        "t1 should complete same tick: r1={r1:?}"
    );

    let r2 = runner.tick_project(project.id).await.unwrap();
    assert!(
        r2.spawned >= 1 || r2.runs_terminated >= 1 || r2.completed_via_runtime >= 1,
        "r2={r2:?}"
    );

    // Extra tick for rollup if t2 completed at end of r2 without rollup edge case
    let r3 = runner.tick_project(project.id).await.unwrap();

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "completed", "r1={r1:?} r2={r2:?} r3={r3:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(
        snap.tasks.iter().all(|t| t.status == "done_success"),
        "tasks={:?}",
        snap.tasks
            .iter()
            .map(|t| (&t.plan_task_id, &t.status))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn tick_cancel_before_materialize_terminates_run() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-c-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.request_cancel(project.id, run_id).await.unwrap();

    let r = runner(store.clone(), 1000)
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 0);
    assert_eq!(r.spawned, 0);
    assert_eq!(r.runs_terminated, 1);

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "cancelled");
}

/// Docs §15.4: tick must not turn a draft/ready/superseded plan on disk
/// into board tasks. An accepted sibling in the same project still materializes
/// so this is not a tautology that tick never materializes.
#[tokio::test]
async fn tick_does_not_materialize_draft_ready_or_superseded_plans() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-plan-status-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();

    let mut withheld = Vec::new();
    for status in [
        WorkPlanStatus::Draft,
        WorkPlanStatus::Ready,
        WorkPlanStatus::Superseded,
    ] {
        let body = chain_plan(project.id);
        let plan_id = body.plan_id;
        store.insert_accepted_plan(project.id, body).await.unwrap();
        sqlx::query("UPDATE work_plans SET status = $2, accepted_at = NULL WHERE id = $1")
            .bind(plan_id)
            .bind(status.as_db())
            .execute(store.pool())
            .await
            .unwrap();
        withheld.push(plan_id);
    }

    let accepted = chain_plan(project.id);
    let accepted_id = accepted.plan_id;
    store
        .insert_accepted_plan(project.id, accepted)
        .await
        .unwrap();

    let r = runner(store.clone(), 60_000)
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 1, "r={r:?}");
    assert_eq!(r.tasks_materialized, 2, "r={r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);
    assert!(
        snap.tasks.iter().all(|t| t.plan_id == accepted_id),
        "tick must not create board tasks for draft/ready/superseded: {:?}",
        snap.tasks
            .iter()
            .map(|t| (t.plan_id, &t.plan_task_id))
            .collect::<Vec<_>>()
    );

    for plan_id in withheld {
        let materialized_at: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT materialized_at FROM work_plans WHERE id = $1")
                .bind(plan_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(
            materialized_at.is_none(),
            "non-accepted plan {plan_id} must stay unmaterialized"
        );
    }
}

#[tokio::test]
async fn tick_cancel_mid_run_drains_tasks() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-mid-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    // Set kill_grace to 0 via raw SQL after create for immediate cancel drain
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    sqlx::query("UPDATE project_configs SET kill_grace_s = 0 WHERE project_id = $1")
        .bind(project.id)
        .execute(store.pool())
        .await
        .unwrap();

    let runner = runner(store.clone(), 60_000); // long-running stub
    let r1 = runner.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1);

    store.request_cancel(project.id, run_id).await.unwrap();
    let r2 = runner.tick_project(project.id).await.unwrap();
    assert!(r2.cancel_tasks + r2.killed + r2.runs_terminated >= 1);

    // Another tick to finish rollup if needed
    let _ = runner.tick_project(project.id).await.unwrap();

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "cancelled");
}

#[tokio::test]
async fn tick_inbox_status_success() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-inbox-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let runner = runner(store.clone(), 60_000);
    runner.tick_project(project.id).await.unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .unwrap();

    store
        .append_worker_report(
            project.id,
            session.id,
            t1.id,
            1,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();

    let r = runner.tick_project(project.id).await.unwrap();
    assert!(r.inbox_applied >= 1);

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success");
}

/// Docs: a tick while the project advisory lock is held is Conflict, not
/// `Ok(empty TickReport)` (which HTTP would 200 with `spawned=0`).
#[tokio::test]
async fn tick_while_project_lock_held_is_conflict_not_empty_ok() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-lock-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    store
        .insert_accepted_plan(project.id, chain_plan(project.id))
        .await
        .unwrap();

    let mut lock_conn = store.lock_project(project.id).await.unwrap();
    let runner = runner(store.clone(), 60_000);
    let result = runner.tick_project(project.id).await;
    match result {
        Err(TickError::Store(StoreError::Conflict(msg))) => {
            let lc = msg.to_ascii_lowercase();
            assert!(
                lc.contains("lock")
                    || lc.contains("already running")
                    || lc.contains("tick_lock_held"),
                "Conflict must name the held tick lock: {msg}"
            );
        }
        Ok(report) => {
            panic!("must not swallow StoreError::Conflict as Ok(empty TickReport): {report:?}")
        }
        Err(other) => panic!("expected StoreError::Conflict, got {other:?}"),
    }

    Store::unlock_project(&mut lock_conn, project.id)
        .await
        .unwrap();

    // Control: after release, the same runner must actually tick (not a
    // tautology that every tick is Conflict).
    let ok = runner.tick_project(project.id).await.unwrap();
    assert!(
        ok.plans_materialized >= 1 || ok.tasks_materialized >= 1 || ok.spawned >= 1,
        "tick after unlock must do work: {ok:?}"
    );
}

fn plan_task(id: &str, title: &str) -> PlanTask {
    PlanTask {
        plan_task_id: id.into(),
        title: title.into(),
        summary: String::new(),
        spec_md: String::new(),
        depends_on: vec![],
        worker_profile: "coding".into(),
        acceptance: vec![],
        priority: 100,
        external_ref: None,
        timeout_s: Some(3600),
        max_retries: Some(1),
    }
}

/// Two independent ready tasks (no DAG). Higher priority (`high`) sorts first.
fn independent_pair_plan(project_id: Uuid) -> WorkPlanBody {
    let mut high = plan_task("high", "Independent high");
    high.priority = 200;
    let mut low = plan_task("low", "Independent low");
    low.priority = 10;
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![high, low],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

/// Docs §8.4: Extra ready tasks stay ready until a slot frees.
/// No Kubernetes required for queueing. max_agents=1 + two independent
/// ready tasks: one tick spawns one session; the other stays ready
/// (not assigned, not failed_spawn). `free_slots` unit is not enough —
/// spawn must honor it. Distinct from plane-hold (`tick_honors_plane_max_concurrent`),
/// host-at-cap picker, and pick-at-cap-selects-other-host.
#[tokio::test]
async fn tick_capacity_extra_ready_tasks_stay_ready() {
    let store = setup().await;

    // Control: both independent tasks spawn when there are two slots.
    // Without this, "spawn one per tick" would fake the max_agents=1 case.
    let roomy = store
        .create_project(&format!("tick-cap-room-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    store
        .insert_accepted_plan(roomy.id, independent_pair_plan(roomy.id))
        .await
        .unwrap();
    let roomy_tick = runner(store.clone(), 60_000)
        .tick_project(roomy.id)
        .await
        .unwrap();
    assert_eq!(
        roomy_tick.spawned, 2,
        "two independent ready tasks at max_agents=2 must both spawn (not one-per-tick): {roomy_tick:?}"
    );
    assert_eq!(roomy_tick.spawn_failed, 0, "{roomy_tick:?}");
    let roomy_snap = store.load_board_snapshot(roomy.id).await.unwrap();
    assert_eq!(roomy_snap.sessions.len(), 2);
    assert!(
        roomy_snap.tasks.iter().all(|t| t.status == "in_progress"),
        "both ready tasks must take a slot when one is free: {:?}",
        roomy_snap
            .tasks
            .iter()
            .map(|t| (&t.plan_task_id, &t.status))
            .collect::<Vec<_>>()
    );
    assert!(
        roomy_snap
            .sessions
            .iter()
            .all(|s| s.runtime_backend == "stub" && s.status != "failed_spawn"),
        "queueing is stub/local — no Kubernetes: {:?}",
        roomy_snap
            .sessions
            .iter()
            .map(|s| (&s.runtime_backend, &s.status))
            .collect::<Vec<_>>()
    );

    let project = store
        .create_project(&format!("tick-cap-{}", Uuid::new_v4()), 1)
        .await
        .unwrap();
    let cfg = store.get_project_config(project.id).await.unwrap();
    assert_eq!(cfg.max_agents, 1, "fixture must be max_agents=1");
    store
        .insert_accepted_plan(project.id, independent_pair_plan(project.id))
        .await
        .unwrap();

    let runner = runner(store.clone(), 60_000);
    let r1 = runner.tick_project(project.id).await.unwrap();
    assert_eq!(
        r1.spawned, 1,
        "max_agents=1 must spawn exactly one of two independent ready tasks: {r1:?}"
    );
    assert_eq!(
        r1.spawn_failed, 0,
        "extra must stay ready, not fail spawn: {r1:?}"
    );

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let high = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "high")
        .expect("high");
    let low = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "low")
        .expect("low");
    assert_eq!(
        high.status, "in_progress",
        "higher-priority ready task takes the only slot: {:?}",
        high.status
    );
    assert!(
        high.claimed_session_id.is_some(),
        "spawned task must have a session claim"
    );
    assert_eq!(
        low.status, "ready",
        "extra ready task must stay ready (not assigned, not failed): {:?}",
        low.status
    );
    assert!(
        low.claimed_session_id.is_none(),
        "extra must not be assigned a session: {:?}",
        low.claimed_session_id
    );
    assert!(
        low.assigned_at.is_none(),
        "extra must not have been assigned: {:?}",
        low.assigned_at
    );
    assert_eq!(
        snap.sessions.len(),
        1,
        "one slot → one session: {:?}",
        snap.sessions
            .iter()
            .map(|s| (&s.status, s.board_task_id))
            .collect::<Vec<_>>()
    );
    let live = &snap.sessions[0];
    assert_eq!(live.board_task_id, high.id);
    assert_ne!(
        live.status, "failed_spawn",
        "the spawned session must not be failed_spawn: {:?}",
        live.status
    );
    assert_eq!(
        live.runtime_backend, "stub",
        "Docs §8.4: no Kubernetes required for queueing, got {}",
        live.runtime_backend
    );

    // Slot still held: a second tick must not take the extra.
    let r2 = runner.tick_project(project.id).await.unwrap();
    assert_eq!(
        r2.spawned, 0,
        "held max_agents slot must not spawn the extra on the next tick: {r2:?}"
    );
    assert_eq!(r2.spawn_failed, 0, "{r2:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let low = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "low")
        .expect("low");
    let high = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "high")
        .expect("high");
    assert_eq!(
        low.status, "ready",
        "extra must stay ready while the slot is held: {:?}",
        low.status
    );
    assert!(low.claimed_session_id.is_none());
    assert_eq!(high.status, "in_progress");
    assert_eq!(snap.sessions.len(), 1);
    assert!(
        !snap.sessions.iter().any(|s| s.status == "failed_spawn"),
        "extra staying ready is not a failed_spawn: {:?}",
        snap.sessions.iter().map(|s| &s.status).collect::<Vec<_>>()
    );

    // Slot frees: complete the live session; the extra must then spawn.
    let live = snap.sessions.first().expect("live session");
    store
        .append_worker_report(
            project.id,
            live.id,
            high.id,
            1,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();
    let r3 = runner.tick_project(project.id).await.unwrap();
    assert!(
        r3.inbox_applied >= 1,
        "success report must apply so the slot frees: {r3:?}"
    );
    assert_eq!(
        r3.spawned, 1,
        "once the slot frees, the extra ready task must spawn: {r3:?}"
    );
    assert_eq!(r3.spawn_failed, 0, "{r3:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let high = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "high")
        .expect("high");
    let low = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "low")
        .expect("low");
    assert_eq!(
        high.status, "done_success",
        "completed first task must free its slot: {:?}",
        high.status
    );
    assert_eq!(
        low.status, "in_progress",
        "extra must leave ready only after a slot frees: {:?}",
        low.status
    );
    assert!(low.claimed_session_id.is_some());
    let extra_sess = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == low.id)
        .expect("extra session");
    assert_ne!(extra_sess.status, "failed_spawn");
    assert_eq!(extra_sess.runtime_backend, "stub");
}

fn wipe_plan(project_id: Uuid) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![
            plan_task("live", "Healthy live"),
            plan_task("just_exited", "Just exited"),
            plan_task("grace_exited", "Exited 14m ago"),
            plan_task("old_exited", "Exited 16m ago"),
            plan_task("just_failed", "Just failed_spawn"),
            plan_task("old_failed", "failed_spawn 16m ago"),
        ],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

fn session_dir(data_dir: &Path, project_id: Uuid, session_id: Uuid) -> PathBuf {
    data_dir
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(session_id.to_string())
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn require_git() {
    assert!(
        Command::new("git").arg("--version").output().is_ok(),
        "git is required for workdir wipe tests (install git; see install-full-suite-deps.sh)"
    );
}

fn init_source_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let _ = Command::new("git")
        .args(["-C", path.to_str().unwrap(), "init", "-q"])
        .status();
    git(path, &["config", "user.email", "t@t.t"]);
    git(path, &["config", "user.name", "t"]);
    std::fs::write(path.join("README.md"), "wipe-source\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-qm", "init"]);
}

fn attach_session_worktree(bare: &Path, session_work: &Path) {
    let dest = session_work.join("repo");
    assert!(
        !dest.exists(),
        "worktree dest must be empty before git worktree add: {}",
        dest.display()
    );
    let out = Command::new("git")
        .args([
            "-C",
            &bare.to_string_lossy(),
            "worktree",
            "add",
            "--detach",
            &dest.to_string_lossy(),
        ])
        .output()
        .unwrap_or_else(|e| panic!("git worktree add: {e}"));
    assert!(
        out.status.success(),
        "git worktree add {} failed: {}",
        dest.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dest.join("README.md").is_file(),
        "attached worktree must contain the source checkout at {}",
        dest.display()
    );
}

fn worktree_list(bare: &Path) -> String {
    let out = Command::new("git")
        .args(["-C", &bare.to_string_lossy(), "worktree", "list"])
        .output()
        .unwrap_or_else(|e| panic!("git worktree list: {e}"));
    assert!(
        out.status.success(),
        "git worktree list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn worktree_listed(list: &str, session_work: &Path) -> bool {
    let repo = session_work.join("repo");
    list.lines().any(|line| {
        line.contains(&repo.to_string_lossy().into_owned())
            || line.contains(&session_work.to_string_lossy().into_owned())
    })
}

async fn session_status(store: &Store, session_id: Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM worker_sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    row.0
}

async fn plant_session(
    store: &Store,
    project_id: Uuid,
    run_id: Uuid,
    task_id: Uuid,
    data_dir: &Path,
    status: &str,
    exited_age_minutes: Option<i32>,
) -> (Uuid, PathBuf) {
    let session_id = Uuid::new_v4();
    let dir = session_dir(data_dir, project_id, session_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("artifact.txt"), format!("{status}:{}", session_id)).unwrap();
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id,
            run_id,
            board_task_id: task_id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: format!("tok-{session_id}"),
            worker_profile: "coding".into(),
            work_dir: Some(dir.to_string_lossy().into_owned()),
        })
        .await
        .unwrap();
    match (status, exited_age_minutes) {
        ("healthy", _) => {
            sqlx::query(
                r#"
                UPDATE worker_sessions
                SET status = 'healthy',
                    last_heartbeat_at = now(),
                    started_at = now(),
                    exited_at = NULL
                WHERE id = $1
                "#,
            )
            .bind(session_id)
            .execute(store.pool())
            .await
            .unwrap();
        }
        (_, Some(mins)) => {
            sqlx::query(
                r#"
                UPDATE worker_sessions
                SET status = $2,
                    exited_at = now() - ($3::text || ' minutes')::interval
                WHERE id = $1
                "#,
            )
            .bind(session_id)
            .bind(status)
            .bind(mins.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        }
        (_, None) => {
            sqlx::query(
                r#"
                UPDATE worker_sessions
                SET status = $2,
                    exited_at = now()
                WHERE id = $1
                "#,
            )
            .bind(session_id)
            .bind(status)
            .execute(store.pool())
            .await
            .unwrap();
        }
    }
    (session_id, dir)
}

/// Docs §8.3 step 15 / §6.4: wipe `exited`|`failed_spawn` workdirs older
/// than 15 minutes (detach git worktree first). A just-exited dir survives
/// the next tick. Backdated `exited_at` ≥15m is deleted. Live/healthy is
/// never wiped.
#[tokio::test]
async fn tick_wipes_exited_failed_spawn_workdirs_only_after_15m() {
    require_git();
    let store = setup().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let project = store
        .create_project(&format!("tick-wipe-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = wipe_plan(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let task = |id: &str| {
        snap.tasks
            .iter()
            .find(|t| t.plan_task_id == id)
            .unwrap_or_else(|| panic!("missing task {id}"))
            .id
    };

    let (live_id, live_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("live"),
        data_dir.path(),
        "healthy",
        None,
    )
    .await;
    let (just_exited_id, just_exited_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("just_exited"),
        data_dir.path(),
        "exited",
        None,
    )
    .await;
    let (_grace_id, grace_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("grace_exited"),
        data_dir.path(),
        "exited",
        Some(14),
    )
    .await;
    let (old_exited_id, old_exited_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("old_exited"),
        data_dir.path(),
        "exited",
        Some(16),
    )
    .await;
    let (_just_failed_id, just_failed_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("just_failed"),
        data_dir.path(),
        "failed_spawn",
        None,
    )
    .await;
    let (old_failed_id, old_failed_dir) = plant_session(
        &store,
        project.id,
        run_id,
        task("old_failed"),
        data_dir.path(),
        "failed_spawn",
        Some(16),
    )
    .await;

    sqlx::query(
        r#"
        UPDATE board_tasks
        SET status = 'in_progress',
            claimed_session_id = $2,
            started_at = now(),
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(task("live"))
    .bind(live_id)
    .execute(store.pool())
    .await
    .unwrap();
    for plan_task_id in [
        "just_exited",
        "grace_exited",
        "old_exited",
        "just_failed",
        "old_failed",
    ] {
        let terminal = if plan_task_id.contains("failed") {
            "done_failed"
        } else {
            "done_success"
        };
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = $2,
                claimed_session_id = NULL,
                terminal_at = now(),
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(task(plan_task_id))
        .bind(terminal)
        .execute(store.pool())
        .await
        .unwrap();
    }

    let src = data_dir.path().join("git-src");
    init_source_repo(&src);
    let project_dir = data_dir
        .path()
        .join("projects")
        .join(project.id.to_string());
    std::fs::create_dir_all(&project_dir).unwrap();
    let bare = project_dir.join("source.git");
    let clone = Command::new("git")
        .args([
            "clone",
            "--bare",
            "--quiet",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ])
        .output()
        .unwrap_or_else(|e| panic!("git clone --bare: {e}"));
    assert!(
        clone.status.success(),
        "git clone --bare failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    for dir in [
        &live_dir,
        &just_exited_dir,
        &grace_dir,
        &old_exited_dir,
        &just_failed_dir,
        &old_failed_dir,
    ] {
        attach_session_worktree(&bare, dir);
    }

    let handoff = data_dir
        .path()
        .join("projects")
        .join(project.id.to_string())
        .join("runs")
        .join(run_id.to_string())
        .join("handoffs");
    std::fs::create_dir_all(&handoff).unwrap();
    std::fs::write(handoff.join("result.md"), "archived before wipe\n").unwrap();

    let runner = TickRunner::new(
        store.clone(),
        Arc::new(StubRuntime::new(Duration::from_secs(60))),
        data_dir.path().to_path_buf(),
        "http://127.0.0.1:8080",
    );
    runner.tick_project(project.id).await.unwrap();

    let live_status = session_status(&store, live_id).await;
    assert_eq!(
        live_status, "healthy",
        "live session must stay healthy; wipe must not select it"
    );
    assert!(
        live_dir.join("artifact.txt").is_file(),
        "live/healthy session workdir must never be wiped: {}",
        live_dir.display()
    );
    assert!(
        just_exited_dir.join("artifact.txt").is_file(),
        "just-exited session dir must survive the next tick: {}",
        just_exited_dir.display()
    );
    assert!(
        grace_dir.join("artifact.txt").is_file(),
        "exited_at 14m ago is still inside the 15m grace: {}",
        grace_dir.display()
    );
    assert!(
        !old_exited_dir.exists(),
        "exited_at 16m ago (≥15m) must delete the session dir: {}",
        old_exited_dir.display()
    );
    assert!(
        just_failed_dir.join("artifact.txt").is_file(),
        "just-failed_spawn dir must survive the 15m grace: {}",
        just_failed_dir.display()
    );
    assert!(
        !old_failed_dir.exists(),
        "failed_spawn exited_at 16m ago must delete the session dir: {}",
        old_failed_dir.display()
    );
    assert_eq!(
        session_status(&store, old_exited_id).await,
        "exited",
        "wipe deletes the dir, not the exited session row"
    );
    assert_eq!(
        session_status(&store, old_failed_id).await,
        "failed_spawn",
        "wipe deletes the dir, not the failed_spawn session row"
    );
    assert!(
        handoff.join("result.md").is_file(),
        "wipe is session-dir scoped; archived run handoffs must remain"
    );
    assert!(
        bare.join("HEAD").is_file(),
        "detach must not delete the per-project bare cache"
    );

    let listed = worktree_list(&bare);
    assert!(
        worktree_listed(&listed, &live_dir),
        "healthy worktree must stay attached after tick: {listed}"
    );
    assert!(
        worktree_listed(&listed, &just_exited_dir),
        "just-exited worktree must stay attached (not detached on grace): {listed}"
    );
    assert!(
        worktree_listed(&listed, &grace_dir),
        "14m exited worktree must stay attached: {listed}"
    );
    assert!(
        worktree_listed(&listed, &just_failed_dir),
        "just-failed_spawn worktree must stay attached: {listed}"
    );
    assert!(
        !worktree_listed(&listed, &old_exited_dir),
        "wipe must detach the old exited worktree before deleting the dir: {listed}"
    );
    assert!(
        !worktree_listed(&listed, &old_failed_dir),
        "wipe must detach the old failed_spawn worktree before deleting the dir: {listed}"
    );

    sqlx::query(
        r#"
        UPDATE worker_sessions
        SET exited_at = now() - interval '16 minutes'
        WHERE id = $1
        "#,
    )
    .bind(just_exited_id)
    .execute(store.pool())
    .await
    .unwrap();

    runner.tick_project(project.id).await.unwrap();

    assert_eq!(
        session_status(&store, live_id).await,
        "healthy",
        "later wipe tick must still leave the live session healthy"
    );
    assert!(
        live_dir.join("artifact.txt").is_file(),
        "healthy workdir must still exist after a later wipe tick: {}",
        live_dir.display()
    );
    assert!(
        !just_exited_dir.exists(),
        "the same just-exited session must be wiped once exited_at is backdated ≥15m: {}",
        just_exited_dir.display()
    );
    assert!(
        grace_dir.join("artifact.txt").is_file(),
        "14m grace session must still survive the second tick: {}",
        grace_dir.display()
    );
    let listed2 = worktree_list(&bare);
    assert!(
        worktree_listed(&listed2, &live_dir),
        "healthy worktree must remain after the second tick: {listed2}"
    );
    assert!(
        !worktree_listed(&listed2, &just_exited_dir),
        "second tick must detach the now-old just-exited worktree: {listed2}"
    );
}
