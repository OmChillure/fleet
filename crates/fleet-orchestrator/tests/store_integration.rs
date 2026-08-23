//! Store integration tests. Requires Postgres:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable cargo test --test store_integration

use fleet_orchestrator::domain::{
    stable_board_task_id, OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::store::{connect, migrate, Store};
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

fn sample_body(project_id: Uuid) -> WorkPlanBody {
    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    WorkPlanBody {
        plan_id,
        project_id,
        run_id,
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
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn create_project_and_materialize_idempotent() {
    let store = setup().await;

    let name = format!("proj-{}", Uuid::new_v4());
    let project = store.create_project(&name, 3).await.unwrap();
    let cfg = store.get_project_config(project.id).await.unwrap();
    assert_eq!(cfg.max_agents, 3);
    assert!(cfg.repo_url.is_none());
    assert_eq!(cfg.git_ref, "main");

    let remote = fleet_orchestrator::domain::ProjectGitRemote {
        url: "https://github.com/acme/app.git".into(),
        git_ref: "develop".into(),
    };
    let cfg = store
        .set_project_git_remote(project.id, Some(&remote))
        .await
        .unwrap();
    assert_eq!(cfg.repo_url.as_deref(), Some("https://github.com/acme/app.git"));
    assert_eq!(cfg.git_ref, "develop");
    let cfg = store.set_project_git_remote(project.id, None).await.unwrap();
    assert!(cfg.repo_url.is_none());
    assert_eq!(cfg.git_ref, "main");

    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let (_run, plan) = store.insert_accepted_plan(project.id, body.clone()).await.unwrap();
    assert_eq!(plan.status, "accepted");
    assert!(plan.materialized_at.is_none());

    let n = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n, 2);

    let n2 = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n2, 0, "second materialize is no-op");

    assert_eq!(cfg.fleet_llm, "off");
    assert_eq!(cfg.fleet_llm_model, "");
    let loaded = store.get_plan_body(plan_id).await.unwrap().expect("plan body");
    assert_eq!(loaded.plan_id, plan_id);
    assert_eq!(loaded.tasks.len(), 2);

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);

    let t1 = stable_board_task_id(project.id, plan_id, "t1");
    let t2 = stable_board_task_id(project.id, plan_id, "t2");
    let row1 = snap.tasks.iter().find(|t| t.id == t1).unwrap();
    let row2 = snap.tasks.iter().find(|t| t.id == t2).unwrap();
    assert_eq!(row1.status, "queued");
    assert_eq!(row2.depends_on, vec![t1]);
}

#[tokio::test]
async fn cancel_before_materialize_yields_zero_cards() {
    let store = setup().await;

    let project = store
        .create_project(&format!("cancel-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.request_cancel(project.id, run_id).await.unwrap();

    let n = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n, 0);
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(snap.tasks.is_empty());
}

#[tokio::test]
async fn worker_report_inbox_idempotent_seq() {
    let store = setup().await;

    let project = store
        .create_project(&format!("rep-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let task = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();

    sqlx::query("UPDATE board_tasks SET status = 'ready' WHERE id = $1")
        .bind(task.id)
        .execute(store.pool())
        .await
        .unwrap();

    let session_id = Uuid::new_v4();
    let ok = store.cas_assign_task(task.id, session_id, 1).await.unwrap();
    assert!(ok);
    let ok2 = store.cas_assign_task(task.id, Uuid::new_v4(), 1).await.unwrap();
    assert!(!ok2, "CAS fails if not ready");

    use fleet_orchestrator::store::NewWorkerSession;
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id: project.id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: "tok-1".into(),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/x".into()),
        })
        .await
        .unwrap();

    store
        .append_worker_report(
            project.id,
            session_id,
            task.id,
            1,
            "heartbeat",
            serde_json::json!({"pct": 10}),
        )
        .await
        .unwrap();
    store
        .append_worker_report(
            project.id,
            session_id,
            task.id,
            1,
            "heartbeat",
            serde_json::json!({"pct": 10}),
        )
        .await
        .unwrap();

    let reports = store.list_unapplied_reports(project.id).await.unwrap();
    assert_eq!(reports.len(), 1);
}

#[tokio::test]
async fn update_fleet_settings_ollama_and_rejects_unknown_as_off() {
    let store = setup().await;
    let project = store
        .create_project(&format!("llm-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();

    let cfg = store
        .update_fleet_settings(project.id, 8, "ollama", "qwen2.5-coder")
        .await
        .unwrap();
    assert_eq!(cfg.max_agents, 8);
    assert_eq!(cfg.fleet_llm, "ollama");
    assert_eq!(cfg.fleet_llm_model, "qwen2.5-coder");

    let cfg = store
        .update_fleet_settings(project.id, 0, "grok", "should-clear")
        .await
        .unwrap();
    assert_eq!(cfg.max_agents, 1, "clamp low");
    assert_eq!(cfg.fleet_llm, "off", "Grok is env-only, store stays off");
    assert_eq!(cfg.fleet_llm_model, "");

    let again = store.get_project_config(project.id).await.unwrap();
    assert_eq!(again.fleet_llm, "off");
    assert_eq!(again.max_agents, 1);
}

#[tokio::test]
async fn unused_speculative_tables_are_dropped() {
    let store = setup().await;
    for name in [
        "project_leases",
        "idempotency_keys",
        "orchestration_events",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = $1
            )",
        )
        .bind(name)
        .fetch_one(store.pool())
        .await
        .unwrap_or_else(|e| panic!("table exists check for {name}: {e}"));
        assert!(!exists, "{name} is unused and must not exist after migrate");
    }
}