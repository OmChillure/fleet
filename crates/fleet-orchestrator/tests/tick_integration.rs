//! Tick integration: materialize, deps, host assign, inbox, cancel, rollup.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{Store, connect, migrate};
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
    TickRunner::with_store(store)
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("ti{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("tick-int")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_online_host(store: &Store, workspace_id: Uuid) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "tick-host".to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        "tick-host",
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
        Some("tick-host"),
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
async fn tick_materializes_and_holds_ready_without_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    store
        .insert_accepted_plan(project.id, chain_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 1);
    assert_eq!(r.tasks_materialized, 2);
    assert_eq!(r.dep_updates, 1);
    assert_eq!(r.spawned, 0, "no host → do not spawn: {r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "ready");
    assert_eq!(t2.status, "queued");
    assert!(snap.sessions.is_empty());
}

#[tokio::test]
async fn tick_host_assign_and_inbox_completes_chain() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-e2e-{}", Uuid::new_v4()), 3)
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

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("t1 session");
    assert_eq!(session.runtime_backend, "remote");
    store
        .append_worker_report(
            project.id,
            session.id,
            t1.id,
            1,
            "status",
            serde_json::json!({
                "status": "success",
                "result_md": "Task completed successfully with artifacts.",
                "files": {"work/out.txt": "ok"}
            }),
        )
        .await
        .unwrap();

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert!(r2.inbox_applied >= 1, "r2={r2:?}");
    assert!(r2.spawned >= 1, "t2 should assign: r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    let s2 = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t2.id)
        .expect("t2 session");
    store
        .append_worker_report(
            project.id,
            s2.id,
            t2.id,
            1,
            "status",
            serde_json::json!({
                "status": "success",
                "result_md": "Task completed successfully with artifacts.",
                "files": {"work/out.txt": "ok"}
            }),
        )
        .await
        .unwrap();

    let _ = tick.tick_project(project.id).await.unwrap();
    let _ = tick.tick_project(project.id).await.unwrap();

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "completed");
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

    let r = runner(store.clone())
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
    assert!(r2.cancel_tasks + r2.killed + r2.runs_terminated >= 1);

    let _ = tick.tick_project(project.id).await.unwrap();

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
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    tick.tick_project(project.id).await.unwrap();

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
            serde_json::json!({
                "status": "success",
                "result_md": "Task completed successfully with artifacts.",
                "files": {"work/out.txt": "ok"}
            }),
        )
        .await
        .unwrap();

    let r = tick.tick_project(project.id).await.unwrap();
    assert!(r.inbox_applied >= 1);

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success");
}
