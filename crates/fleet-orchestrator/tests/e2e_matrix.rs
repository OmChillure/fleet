//! PR9 E2E matrix (design MVP DoD).
//!
//! Covers:
//! - happy dependency chain
//! - fail-and-retry (stale kill → retry_wait → ready → success)
//! - cancel mid-run
//! - blocked + HITL resume intent
//! - double force-tick idempotency (no double spawn / re-materialize)
//!
//! Requires `DATABASE_URL`.

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

fn single_task(project_id: Uuid, max_retries: i32) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: "Solo".into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(max_retries),
        }],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
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

async fn run_status(store: &Store, run_id: Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    row.0
}

/// Happy path: two-task chain completes via stub duration 0.
#[tokio::test]
async fn e2e_happy_dependency_chain() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-happy-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone(), 0);
    for _ in 0..4 {
        tick.tick_project(project.id).await.unwrap();
    }

    assert_eq!(run_status(&store, run_id).await, "completed");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);
    assert!(snap.tasks.iter().all(|t| t.status == "done_success"));
    assert!(
        snap.sessions.iter().all(|s| s.status == "exited"),
        "sessions should free: {:?}",
        snap.sessions.iter().map(|s| &s.status).collect::<Vec<_>>()
    );
}

/// Fail-and-retry: stale heartbeat kills attempt 1 → retry_wait → ready → success.
#[tokio::test]
async fn e2e_fail_and_retry() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-retry-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = single_task(project.id, 1);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    sqlx::query(
        "UPDATE project_configs SET heartbeat_stale_s = 1, kill_grace_s = 0, retry_backoff_s = 0 WHERE project_id = $1",
    )
    .bind(project.id)
    .execute(store.pool())
    .await
    .unwrap();

    // Long-running stub so we can force stale before natural exit.
    let tick_slow = runner(store.clone(), 60_000);
    let r1 = tick_slow.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "in_progress");
    assert_eq!(t1.attempt, 1);

    tokio::time::sleep(Duration::from_millis(1100)).await;

    let r2 = tick_slow.tick_project(project.id).await.unwrap();
    assert!(
        r2.killed >= 1 || r2.retried >= 1,
        "expected stale kill/retry r2={r2:?}"
    );

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "retry_wait",
        "after stale should be retry_wait, got {t1:?}"
    );

    // Clear backoff so Step 5b promotes immediately.
    sqlx::query(
        "UPDATE board_tasks SET retry_not_before = now() - interval '1 second' WHERE id = $1",
    )
    .bind(t1.id)
    .execute(store.pool())
    .await
    .unwrap();

    // Zero-duration stub for the second attempt so it completes cleanly.
    let tick_fast = runner(store.clone(), 0);
    let r3 = tick_fast.tick_project(project.id).await.unwrap();
    assert!(
        r3.retry_promoted >= 1 || r3.spawned >= 1 || r3.completed_via_runtime >= 1,
        "r3={r3:?}"
    );
    let _ = tick_fast.tick_project(project.id).await.unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success", "task={t1:?}");
    assert!(t1.attempt >= 2, "should have re-assigned, attempt={}", t1.attempt);
    assert_eq!(run_status(&store, run_id).await, "completed");
}

/// Cancel mid-run drains in-flight work to run cancelled.
#[tokio::test]
async fn e2e_cancel_mid_run() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-cancel-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    sqlx::query("UPDATE project_configs SET kill_grace_s = 0 WHERE project_id = $1")
        .bind(project.id)
        .execute(store.pool())
        .await
        .unwrap();

    let tick = runner(store.clone(), 60_000);
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1);

    store.request_cancel(project.id, run_id).await.unwrap();
    let r2 = tick.tick_project(project.id).await.unwrap();
    assert!(
        r2.cancel_tasks + r2.killed + r2.runs_terminated >= 1,
        "r2={r2:?}"
    );
    let _ = tick.tick_project(project.id).await.unwrap();

    assert_eq!(run_status(&store, run_id).await, "cancelled");
}

/// Blocked worker + HITL resume intent → re-queue → complete.
#[tokio::test]
async fn e2e_blocked_and_resume() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-hitl-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = single_task(project.id, 0);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick_slow = runner(store.clone(), 60_000);
    tick_slow.tick_project(project.id).await.unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("session");

    store
        .append_worker_report(
            project.id,
            session.id,
            t1.id,
            1,
            "status",
            serde_json::json!({"status": "blocked", "reason": "needs_approval"}),
        )
        .await
        .unwrap();

    let r_block = tick_slow.tick_project(project.id).await.unwrap();
    assert!(r_block.inbox_applied >= 1, "r_block={r_block:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "blocked");
    assert_eq!(run_status(&store, run_id).await, "active");

    let ok = store
        .request_hitl_resume(project.id, t1.id, "approved by operator")
        .await
        .unwrap();
    assert!(ok, "resume intent should stick while blocked");

    // Fast stub after resume so second attempt completes.
    let tick_fast = runner(store.clone(), 0);
    let r_resume = tick_fast.tick_project(project.id).await.unwrap();
    // apply_hitl_resume promotes blocked→ready; same or next tick may spawn.
    assert!(
        r_resume.spawned >= 1
            || r_resume.completed_via_runtime >= 1
            || store
                .load_board_snapshot(project.id)
                .await
                .unwrap()
                .tasks
                .iter()
                .any(|t| t.plan_task_id == "t1" && t.status != "blocked"),
        "r_resume={r_resume:?}"
    );
    for _ in 0..3 {
        tick_fast.tick_project(project.id).await.unwrap();
    }

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success", "task={t1:?}");
    assert_eq!(run_status(&store, run_id).await, "completed");
}

/// Double tick after a complete board must not re-materialize or re-spawn.
#[tokio::test]
async fn e2e_double_force_tick_idempotent() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-idemp-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone(), 0);
    for _ in 0..4 {
        tick.tick_project(project.id).await.unwrap();
    }
    assert_eq!(run_status(&store, run_id).await, "completed");

    let sessions_before = store
        .load_board_snapshot(project.id)
        .await
        .unwrap()
        .sessions
        .len();

    let r_a = tick.tick_project(project.id).await.unwrap();
    let r_b = tick.tick_project(project.id).await.unwrap();

    assert_eq!(r_a.plans_materialized, 0, "r_a={r_a:?}");
    assert_eq!(r_a.tasks_materialized, 0, "r_a={r_a:?}");
    assert_eq!(r_a.spawned, 0, "r_a={r_a:?}");
    assert_eq!(r_b.plans_materialized, 0, "r_b={r_b:?}");
    assert_eq!(r_b.tasks_materialized, 0, "r_b={r_b:?}");
    assert_eq!(r_b.spawned, 0, "r_b={r_b:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);
    assert_eq!(snap.sessions.len(), sessions_before);
    assert_eq!(run_status(&store, run_id).await, "completed");
}

/// Cancel before first tick → zero board cards, run cancelled.
#[tokio::test]
async fn e2e_cancel_before_materialize() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-c0-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.request_cancel(project.id, run_id).await.unwrap();

    let r = runner(store.clone(), 0)
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 0);
    assert_eq!(r.tasks_materialized, 0);
    assert_eq!(run_status(&store, run_id).await, "cancelled");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(snap.tasks.is_empty(), "expected zero board_tasks");
}
