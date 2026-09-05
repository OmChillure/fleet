//! Store integration tests. Requires Postgres:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://fleet:fleet@localhost:5432/fleet?sslmode=disable cargo test --test store_integration

use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus, stable_board_task_id,
};
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
    assert_eq!(
        cfg.repo_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    assert_eq!(cfg.git_ref, "develop");
    let cfg = store
        .set_project_git_remote(project.id, None)
        .await
        .unwrap();
    assert!(cfg.repo_url.is_none());
    assert_eq!(cfg.git_ref, "main");

    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let (_run, plan) = store
        .insert_accepted_plan(project.id, body.clone())
        .await
        .unwrap();
    assert_eq!(plan.status, "accepted");
    assert!(plan.materialized_at.is_none());

    let n = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n, 2);

    let n2 = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n2, 0, "second materialize is no-op");

    assert_eq!(cfg.fleet_llm, "off");
    assert_eq!(cfg.fleet_llm_model, "");
    let loaded = store
        .get_plan_body(plan_id)
        .await
        .unwrap()
        .expect("plan body");
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
    let ok2 = store
        .cas_assign_task(task.id, Uuid::new_v4(), 1)
        .await
        .unwrap();
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
async fn live_events_persist_and_duplicate_seq_is_noop() {
    let store = setup().await;
    let project = store
        .create_project(&format!("live-{}", Uuid::new_v4()), 2)
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
    assert!(store.cas_assign_task(task.id, session_id, 1).await.unwrap());
    store
        .insert_session_spawning(&fleet_orchestrator::store::NewWorkerSession {
            id: session_id,
            project_id: project.id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "remote".into(),
            worker_token: "tok-live".into(),
            worker_profile: "coding".into(),
            work_dir: Some("/plane/only".into()),
        })
        .await
        .unwrap();

    let ts = chrono::Utc::now();
    let n = store
        .insert_live_events(
            session_id,
            7,
            &[(ts, "update".into(), "built calculator".into())],
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    let n2 = store
        .insert_live_events(
            session_id,
            7,
            &[(ts, "update".into(), "should not appear twice".into())],
        )
        .await
        .unwrap();
    assert_eq!(n2, 0, "duplicate report_seq must not append");

    let live = store.list_live_events(session_id, 50).await.unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0]["text"], "built calculator");
    assert_eq!(live[0]["kind"], "update");

    let n3 = store
        .insert_live_events(
            session_id,
            8,
            &[
                (ts, "update".into(), "the".into()),
                (ts, "update".into(), "the".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(n3, 2);
    let act = store
        .list_recent_live_activity(&[session_id], 40)
        .await
        .unwrap();
    assert_eq!(act.len(), 3);
    let keys: std::collections::HashSet<String> = act
        .iter()
        .map(|e| format!("{}-{}-{}", e["session_id"], e["report_seq"], e["idx"]))
        .collect();
    assert_eq!(
        keys.len(),
        act.len(),
        "activity rows must be uniquely keyed"
    );
}

#[tokio::test]
async fn replace_does_not_leave_sibling_or_accept_dead_heartbeat() {
    let store = setup().await;
    let project = store
        .create_project(&format!("repl-{}", Uuid::new_v4()), 2)
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

    let first = store
        .begin_spawn_assignment(
            task.id,
            project.id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/a"),
        )
        .await
        .unwrap()
        .expect("first spawn");
    store
        .apply_heartbeat(first.session_id, task.id, 1, Some(40))
        .await
        .unwrap();

    let retired = store
        .retire_live_sessions(task.id, None, "worker_replaced")
        .await
        .unwrap();
    assert_eq!(retired.len(), 1);
    assert!(
        !store
            .complete_spawn_failure(first.session_id)
            .await
            .unwrap()
    );

    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(task.id)
        .execute(store.pool())
        .await
        .unwrap();
    let second = store
        .begin_spawn_assignment(
            task.id,
            project.id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/b"),
        )
        .await
        .unwrap()
        .expect("second spawn");
    let retired2 = store
        .retire_live_sessions(task.id, Some(second.session_id), "superseded")
        .await
        .unwrap();
    assert!(retired2.is_empty(), "first session already exited");

    store
        .apply_heartbeat(first.session_id, task.id, 9, Some(99))
        .await
        .unwrap();
    let old = store.get_session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, "exited");
    assert_eq!(old.exit_reason.as_deref(), Some("worker_replaced"));

    let task_now = store.get_task(project.id, task.id).await.unwrap().unwrap();
    assert_eq!(task_now.claimed_session_id, Some(second.session_id));
    assert_ne!(task_now.progress_pct, 99);

    assert!(
        store
            .live_claim_is_other_attempt(task.id, first.attempt)
            .await
            .unwrap()
    );
    assert!(store.task_has_live_session(task.id).await.unwrap());
}

#[tokio::test]
async fn complete_spawn_failure_voids_claims_and_rejects_late_worker() {
    let store = setup().await;
    let project = store
        .create_project(&format!("spawn-fail-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let slug = format!("sf{}", project.id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project.id)
    .bind(&slug)
    .bind("spawn-fail")
    .execute(store.pool())
    .await
    .expect("insert workspace");

    let body = sample_body(project.id);
    let plan_id = body.plan_id;
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
    assert!(store.cas_assign_task(task.id, session_id, 1).await.unwrap());
    store
        .insert_session_spawning(&fleet_orchestrator::store::NewWorkerSession {
            id: session_id,
            project_id: project.id,
            run_id: task.run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "remote".into(),
            worker_token: "tok-late".into(),
            worker_profile: "coding".into(),
            work_dir: None,
        })
        .await
        .unwrap();

    let claim = fleet_db::NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id: project.id,
        kind: "spawn_session".into(),
        payload: serde_json::json!({
            "host_id": Uuid::now_v7(),
            "session_id": session_id,
        }),
        max_attempts: 3,
        available_at: chrono::Utc::now(),
    };
    fleet_db::repos::work_claims::enqueue(store.pool(), &claim)
        .await
        .expect("enqueue spawn claim");

    let flipped = store
        .complete_spawn_failure(session_id)
        .await
        .expect("complete_spawn_failure");
    assert!(flipped, "spawning session must become failed_spawn");

    let dead = fleet_db::repos::work_claims::get(store.pool(), claim.id)
        .await
        .expect("get claim")
        .expect("claim row");
    assert_eq!(
        dead.status, "dead",
        "timed-out spawn must not stay leaseable"
    );

    let session = store.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.status, "failed_spawn");

    let authed = store
        .authenticate_worker_session(session_id, "tok-late")
        .await
        .expect("auth");
    assert!(
        authed.is_none(),
        "failed_spawn session must not accept worker heartbeats"
    );
}
