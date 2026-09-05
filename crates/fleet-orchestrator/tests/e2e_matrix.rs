//! E2E matrix against the host-assign path (not an in-process plane runtime).
//!
//! Covers:
//! - happy dependency chain
//! - fail-and-retry (stale kill → retry_wait → ready → success)
//! - cancel mid-run
//! - blocked + HITL resume
//! - double force-tick idempotency
//!
//! Requires `DATABASE_URL`.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{connect, migrate, Store};
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
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    Store::new(pool)
}

fn runner(store: Store) -> TickRunner {
    // Not loopback: these tests pair a fixture host (`e2e-host`), not this
    // machine. Loopback would drop that host and starve the chain.
    TickRunner::new(
        store,
        std::env::temp_dir().join("fleet-orch-e2e"),
        "http://192.0.2.10:8080",
    )
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("e2{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("e2e")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_online_host(store: &Store, workspace_id: Uuid) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "e2e-host".to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        "e2e-host",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("pair");
    worker_hosts::heartbeat(
        store.pool(),
        host.id,
        true,
        false,
        Some("linux-x86_64"),
        Some("e2e-host"),
        None,
        0,
        Some(4),
        Some("abc"),
        None,
        Some(false),
        None,
    )
    .await
    .expect("heartbeat");
    host.id
}

async fn report_success(store: &Store, project_id: Uuid, plan_task_id: &str) {
    let snap = store.load_board_snapshot(project_id).await.unwrap();
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == plan_task_id)
        .unwrap();
    let session = if let Some(sid) = task.claimed_session_id {
        snap.sessions.iter().find(|s| s.id == sid)
    } else {
        snap.sessions
            .iter()
            .filter(|s| s.board_task_id == task.id)
            .max_by_key(|s| s.attempt)
    }
    .unwrap_or_else(|| {
        panic!(
            "no session for {plan_task_id} claim={:?} sessions={:?}",
            task.claimed_session_id,
            snap.sessions
                .iter()
                .map(|s| (s.id, s.board_task_id, s.attempt, &s.status))
                .collect::<Vec<_>>()
        )
    });
    store
        .append_worker_report(
            project_id,
            session.id,
            task.id,
            session.attempt,
            "status",
            serde_json::json!({
                "status": "success",
                "result_md": "Task completed successfully with artifacts.",
                "files": {"work/out.txt": "ok"}
            }),
        )
        .await
        .unwrap();
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

#[tokio::test]
async fn e2e_happy_dependency_chain() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-happy-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert!(r1.spawned >= 1, "r1={r1:?}");
    report_success(&store, project.id, "t1").await;
    let r2 = tick.tick_project(project.id).await.unwrap();
    assert!(r2.inbox_applied >= 1 && r2.spawned >= 1, "r2={r2:?}");
    report_success(&store, project.id, "t2").await;
    tick.tick_project(project.id).await.unwrap();
    tick.tick_project(project.id).await.unwrap();

    assert_eq!(run_status(&store, run_id).await, "completed");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(snap.tasks.iter().all(|t| t.status == "done_success"));
}

#[tokio::test]
async fn e2e_fail_and_retry() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-retry-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
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

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

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
            "heartbeat",
            serde_json::json!({"progress_pct": 10}),
        )
        .await
        .unwrap();
    tick.tick_project(project.id).await.unwrap();

    tokio::time::sleep(Duration::from_millis(1100)).await;

    let r2 = tick.tick_project(project.id).await.unwrap();
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

    sqlx::query(
        "UPDATE board_tasks SET retry_not_before = now() - interval '1 second' WHERE id = $1",
    )
    .bind(t1.id)
    .execute(store.pool())
    .await
    .unwrap();

    let r3 = tick.tick_project(project.id).await.unwrap();
    assert!(r3.retry_promoted >= 1 || r3.spawned >= 1, "r3={r3:?}");
    report_success(&store, project.id, "t1").await;
    tick.tick_project(project.id).await.unwrap();
    tick.tick_project(project.id).await.unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success", "task={t1:?}");
    assert!(t1.attempt >= 2, "attempt={}", t1.attempt);
    assert_eq!(run_status(&store, run_id).await, "completed");
}

#[tokio::test]
async fn e2e_cancel_mid_run() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-cancel-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    sqlx::query("UPDATE project_configs SET kill_grace_s = 0 WHERE project_id = $1")
        .bind(project.id)
        .execute(store.pool())
        .await
        .unwrap();

    let tick = runner(store.clone());
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

#[tokio::test]
async fn e2e_blocked_and_resume() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-hitl-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = single_task(project.id, 0);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    tick.tick_project(project.id).await.unwrap();

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

    let r_block = tick.tick_project(project.id).await.unwrap();
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

    let r_resume = tick.tick_project(project.id).await.unwrap();
    assert!(
        r_resume.spawned >= 1
            || store
                .load_board_snapshot(project.id)
                .await
                .unwrap()
                .tasks
                .iter()
                .any(|t| t.plan_task_id == "t1" && t.status != "blocked"),
        "r_resume={r_resume:?}"
    );
    report_success(&store, project.id, "t1").await;
    for _ in 0..3 {
        tick.tick_project(project.id).await.unwrap();
    }

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success", "task={t1:?}");
    assert_eq!(run_status(&store, run_id).await, "completed");
}

#[tokio::test]
async fn e2e_double_force_tick_idempotent() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-idemp-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    let _ = tick.tick_project(project.id).await.unwrap();
    report_success(&store, project.id, "t1").await;
    let _ = tick.tick_project(project.id).await.unwrap();
    report_success(&store, project.id, "t2").await;
    tick.tick_project(project.id).await.unwrap();
    tick.tick_project(project.id).await.unwrap();
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

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 0);
    assert_eq!(r.tasks_materialized, 0);
    assert_eq!(r.spawned, 0);
    assert_eq!(run_status(&store, run_id).await, "cancelled");
}
