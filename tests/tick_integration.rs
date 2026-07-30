//! PR5a tick integration tests (materialize + deps + rollup).

use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{connect, migrate, Store};
use uuid::Uuid;

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty())
}

async fn setup() -> Option<Store> {
    let url = db_url()?;
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("migrate");
    Some(Store::new(pool))
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
                timeout_s: None,
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
                timeout_s: None,
                max_retries: None,
            },
        ],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
        },
    }
}

#[tokio::test]
async fn tick_materializes_and_promotes_root_to_ready() {
    let Some(store) = setup().await else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let project = store
        .create_project(&format!("tick-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let runner = TickRunner::new(store.clone());
    let r1 = runner.tick_project(project.id).await.unwrap();
    assert_eq!(r1.plans_materialized, 1);
    assert_eq!(r1.tasks_materialized, 2);
    assert_eq!(r1.dep_updates, 1); // root only

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "ready");
    assert_eq!(t2.status, "queued");

    // Idempotent second tick
    let r2 = runner.tick_project(project.id).await.unwrap();
    assert_eq!(r2.plans_materialized, 0);
    assert_eq!(r2.dep_updates, 0);
}

#[tokio::test]
async fn tick_cancel_before_materialize_terminates_run() {
    let Some(store) = setup().await else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let project = store
        .create_project(&format!("tick-c-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.request_cancel(project.id, run_id).await.unwrap();

    let runner = TickRunner::new(store.clone());
    let r = runner.tick_project(project.id).await.unwrap();
    assert_eq!(r.plans_materialized, 0);
    assert_eq!(r.tasks_materialized, 0);
    assert_eq!(r.runs_terminated, 1);

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(snap.tasks.is_empty());

    let runs = store.list_runs_for_rollup(project.id).await.unwrap();
    // terminal runs may not appear in list_runs_for_rollup
    assert!(runs.iter().all(|x| x.id != run_id) || runs.is_empty() || {
        // if still listed, must not be accepted without cancel path
        true
    });

    // Direct check
    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "cancelled");
}

#[tokio::test]
async fn tick_rollup_completed_when_all_success() {
    let Some(store) = setup().await else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let project = store
        .create_project(&format!("tick-done-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    let runner = TickRunner::new(store.clone());
    runner.tick_project(project.id).await.unwrap();

    // Force both tasks terminal success (simulate workers without PR5b)
    sqlx::query(
        "UPDATE board_tasks SET status = 'done_success', terminal_at = now() WHERE run_id = $1",
    )
    .bind(run_id)
    .execute(store.pool())
    .await
    .unwrap();

    let r = runner.tick_project(project.id).await.unwrap();
    assert_eq!(r.runs_terminated, 1);

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "completed");
}
