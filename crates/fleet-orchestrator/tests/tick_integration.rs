//! PR5 tick integration tests (materialize, deps, spawn stub, cancel, rollup).

use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::sandbox::StubRuntime;
use fleet_orchestrator::store::{connect, migrate, Store};
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
    assert_eq!(
        row.0, "completed",
        "r1={r1:?} r2={r2:?} r3={r3:?}"
    );

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
    let session = snap.sessions.iter().find(|s| s.board_task_id == t1.id).unwrap();

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
