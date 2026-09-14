//! Store integration tests. Requires Postgres:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable cargo test --test store_integration

use fleet_orchestrator::domain::{
    ExitReason, OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
    stable_board_task_id,
};
use fleet_orchestrator::orch::{apply_retry_after_failure, TickRunner};
use fleet_orchestrator::store::{SpawnAssignment, Store, StoreError, connect, migrate};
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

/// insert_accepted_plan always writes Accepted; plant a non-accepted row.
async fn insert_plan_with_status(store: &Store, body: &WorkPlanBody, status: WorkPlanStatus) {
    sqlx::query(
        r#"
        INSERT INTO work_runs (id, project_id, status, source)
        VALUES ($1, $2, 'accepted', $3)
        "#,
    )
    .bind(body.run_id)
    .bind(body.project_id)
    .bind(&body.source)
    .execute(store.pool())
    .await
    .unwrap();

    let body_json = serde_json::to_value(body).unwrap();
    sqlx::query(
        r#"
        INSERT INTO work_plans (
            id, project_id, run_id, version, status, source, body
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(body.plan_id)
    .bind(body.project_id)
    .bind(body.run_id)
    .bind(body.version)
    .bind(status.as_db())
    .bind(&body.source)
    .bind(body_json)
    .execute(store.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn create_project_and_materialize_idempotent() {
    let store = setup().await;

    let project = store
        .create_project(&format!("proj-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    let (_run, plan) = store
        .insert_accepted_plan(project.id, body)
        .await
        .unwrap();
    assert_eq!(plan.status, "accepted");
    assert!(plan.materialized_at.is_none());

    let n = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n, 2);

    let n2 = store.materialize_plan(project.id, plan_id).await.unwrap();
    assert_eq!(n2, 0, "second materialize is no-op");

    let run = store
        .get_run(project.id, run_id)
        .await
        .unwrap()
        .expect("run");
    assert_eq!(run.status, "active");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);

    let t1 = stable_board_task_id(project.id, plan_id, "t1");
    let t2 = stable_board_task_id(project.id, plan_id, "t2");
    let row1 = snap.tasks.iter().find(|t| t.id == t1).unwrap();
    let row2 = snap.tasks.iter().find(|t| t.id == t2).unwrap();
    assert_eq!(row1.status, "queued");
    assert_eq!(row2.status, "queued");
    assert_eq!(row2.depends_on, vec![t1]);
}

#[tokio::test]
async fn materialize_plan_activates_run_and_stamps_materialized_at() {
    let store = setup().await;
    let project = store
        .create_project(&format!("stamp-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    let (_run, plan) = store
        .insert_accepted_plan(project.id, body)
        .await
        .unwrap();
    assert!(plan.materialized_at.is_none());
    assert!(plan.in_force_at.is_none());

    store.materialize_plan(project.id, plan_id).await.unwrap();

    let run = store
        .get_run(project.id, run_id)
        .await
        .unwrap()
        .expect("run");
    assert_eq!(run.status, "active");

    let (materialized_at, in_force_at): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as("SELECT materialized_at, in_force_at FROM work_plans WHERE id = $1")
        .bind(plan_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(materialized_at.is_some(), "materialized_at must stamp");
    assert!(in_force_at.is_some(), "in_force_at must stamp");
}

#[tokio::test]
async fn materialize_plan_wrong_project_is_not_found() {
    let store = setup().await;
    let project_a = store
        .create_project(&format!("mat-a-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let project_b = store
        .create_project(&format!("mat-b-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project_a.id);
    let plan_id = body.plan_id;
    store
        .insert_accepted_plan(project_a.id, body)
        .await
        .unwrap();

    let err = store
        .materialize_plan(project_b.id, plan_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "cross-project materialize must be NotFound: {err:?}"
    );

    let snap_a = store.load_board_snapshot(project_a.id).await.unwrap();
    let snap_b = store.load_board_snapshot(project_b.id).await.unwrap();
    assert!(snap_a.tasks.is_empty());
    assert!(snap_b.tasks.is_empty());
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
async fn list_pending_materialize_plans_omits_non_accepted() {
    let store = setup().await;
    let project = store
        .create_project(&format!("list-acc-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let draft = sample_body(project.id);
    let ready = sample_body(project.id);
    let accepted = sample_body(project.id);
    let superseded = sample_body(project.id);
    insert_plan_with_status(&store, &draft, WorkPlanStatus::Draft).await;
    insert_plan_with_status(&store, &ready, WorkPlanStatus::Ready).await;
    insert_plan_with_status(&store, &accepted, WorkPlanStatus::Accepted).await;
    insert_plan_with_status(&store, &superseded, WorkPlanStatus::Superseded).await;

    let pending = store
        .list_pending_materialize_plans(project.id)
        .await
        .unwrap();
    assert_eq!(
        pending.iter().map(|p| p.plan_id).collect::<Vec<_>>(),
        vec![accepted.plan_id],
        "only the accepted unmaterialized plan is pending: {pending:?}"
    );
    assert_eq!(pending[0].plan_status, "accepted");
}

#[tokio::test]
async fn materialize_plan_returns_zero_cards_for_non_accepted_status() {
    let store = setup().await;
    let project = store
        .create_project(&format!("mat-nonacc-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    for status in [
        WorkPlanStatus::Draft,
        WorkPlanStatus::Ready,
        WorkPlanStatus::Superseded,
    ] {
        let body = sample_body(project.id);
        let plan_id = body.plan_id;
        insert_plan_with_status(&store, &body, status).await;
        let n = store.materialize_plan(project.id, plan_id).await.unwrap();
        assert_eq!(n, 0, "{status:?} must not materialize");
        let snap = store.load_board_snapshot(project.id).await.unwrap();
        assert!(
            snap.tasks.iter().all(|t| t.plan_id != plan_id),
            "{status:?} must leave board_tasks empty for {plan_id}"
        );
    }
}

/// Materialized t1 + spawning session. Reports and live events only need the session FK.
async fn inbox_session(store: &Store) -> (Uuid, Uuid, Uuid) {
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
    let task_id = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .unwrap()
        .id;
    let session_id = Uuid::new_v4();
    store
        .insert_session_spawning(&fleet_orchestrator::store::NewWorkerSession {
            id: session_id,
            project_id: project.id,
            run_id,
            board_task_id: task_id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: format!("tok-{session_id}"),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/x".into()),
        })
        .await
        .unwrap();
    (project.id, session_id, task_id)
}

#[tokio::test]
async fn worker_report_inbox_idempotent_seq() {
    let store = setup().await;
    let (project_id, session_id, task_id) = inbox_session(&store).await;
    let payload = serde_json::json!({"progress_pct": 10});

    let first = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            1,
            "heartbeat",
            payload.clone(),
        )
        .await
        .unwrap();
    let second = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            1,
            "heartbeat",
            payload.clone(),
        )
        .await
        .unwrap();
    assert!(first, "first seq must insert");
    assert!(!second, "duplicate (session_id, seq) is a no-op");

    let reports = store.list_unapplied_reports(project_id).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].session_id, session_id);
    assert_eq!(reports[0].board_task_id, task_id);
    assert_eq!(reports[0].seq, 1);
    assert_eq!(reports[0].kind, "heartbeat");
    assert_eq!(reports[0].payload, payload);
    assert!(reports[0].applied_at.is_none());
}

#[tokio::test]
async fn append_worker_report_new_seq_is_second_unapplied_row() {
    let store = setup().await;
    let (project_id, session_id, task_id) = inbox_session(&store).await;

    let first = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            1,
            "heartbeat",
            serde_json::json!({"progress_pct": 10}),
        )
        .await
        .unwrap();
    let second = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            2,
            "heartbeat",
            serde_json::json!({"progress_pct": 20}),
        )
        .await
        .unwrap();
    assert!(first);
    assert!(second, "new seq must insert a second inbox row");

    let reports = store.list_unapplied_reports(project_id).await.unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].session_id, session_id);
    assert_eq!(reports[0].seq, 1);
    assert_eq!(reports[0].kind, "heartbeat");
    assert_eq!(reports[0].payload, serde_json::json!({"progress_pct": 10}));
    assert_eq!(reports[1].session_id, session_id);
    assert_eq!(reports[1].seq, 2);
    assert_eq!(reports[1].kind, "heartbeat");
    assert_eq!(reports[1].payload, serde_json::json!({"progress_pct": 20}));
}

#[tokio::test]
async fn append_worker_report_duplicate_seq_keeps_first_payload() {
    let store = setup().await;
    let (project_id, session_id, task_id) = inbox_session(&store).await;
    let first_payload = serde_json::json!({"progress_pct": 10});

    let first = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            1,
            "heartbeat",
            first_payload.clone(),
        )
        .await
        .unwrap();
    let second = store
        .append_worker_report(
            project_id,
            session_id,
            task_id,
            1,
            "heartbeat",
            serde_json::json!({"progress_pct": 99}),
        )
        .await
        .unwrap();
    assert!(first);
    assert!(!second, "ON CONFLICT DO NOTHING must not overwrite");

    let reports = store.list_unapplied_reports(project_id).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].session_id, session_id);
    assert_eq!(reports[0].seq, 1);
    assert_eq!(reports[0].kind, "heartbeat");
    assert_eq!(reports[0].payload, first_payload);
}

#[tokio::test]
async fn cas_assign_task_rejects_second_session_when_not_ready() {
    let store = setup().await;
    let project = store
        .create_project(&format!("cas-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
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

    let first = Uuid::new_v4();
    assert!(store.cas_assign_task(task.id, first, 1).await.unwrap());
    let second = Uuid::new_v4();
    assert!(
        !store.cas_assign_task(task.id, second, 1).await.unwrap(),
        "CAS fails once the task is no longer ready"
    );

    let claimed = store.get_task(project.id, task.id).await.unwrap().unwrap();
    assert_eq!(claimed.claimed_session_id, Some(first));
    assert_ne!(claimed.claimed_session_id, Some(second));
}

#[tokio::test]
async fn update_fleet_settings_ollama_and_rejects_unknown_as_off() {
    let store = setup().await;
    let project = store
        .create_project(&format!("llm-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    // Plant ollama+model so unknown must clear it (CHECK rejects 'grok').
    store
        .update_fleet_settings(project.id, 8, "ollama", "qwen2.5-coder")
        .await
        .unwrap();

    let cfg = store
        .update_fleet_settings(project.id, 8, "grok", "should-clear")
        .await
        .unwrap();
    assert_eq!(cfg.fleet_llm, "off", "Grok is env-only, store stays off");
    assert_eq!(cfg.fleet_llm_model, "");

    let again = store.get_project_config(project.id).await.unwrap();
    assert_eq!(again.fleet_llm, "off");
    assert_eq!(again.fleet_llm_model, "");
}

#[tokio::test]
async fn update_fleet_settings_clamps_max_agents_to_cap() {
    let store = setup().await;
    let project = store
        .create_project(&format!("llm-cap-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();

    let cfg = store
        .update_fleet_settings(
            project.id,
            fleet_orchestrator::domain::MAX_PROJECT_AGENTS + 1,
            "ollama",
            "m",
        )
        .await
        .unwrap();
    assert_eq!(
        cfg.max_agents,
        fleet_orchestrator::domain::MAX_PROJECT_AGENTS
    );
}

#[tokio::test]
async fn update_fleet_settings_truncates_model_to_128() {
    let store = setup().await;
    let project = store
        .create_project(&format!("llm-model-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let model = "x".repeat(200);

    let cfg = store
        .update_fleet_settings(project.id, 2, "ollama", &model)
        .await
        .unwrap();
    assert_eq!(cfg.fleet_llm_model.len(), 128);
    assert_eq!(cfg.fleet_llm_model, "x".repeat(128));
}

#[tokio::test]
async fn update_fleet_settings_normalizes_ollama_case_and_trim() {
    let store = setup().await;
    let project = store
        .create_project(&format!("llm-norm-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();

    let cfg = store
        .update_fleet_settings(project.id, 2, " OLLAMA ", "m")
        .await
        .unwrap();
    assert_eq!(cfg.fleet_llm, "ollama");
}

#[tokio::test]
async fn set_max_agents_does_not_lower_then_raises() {
    let store = setup().await;
    let project = store
        .create_project(&format!("raise-{}", Uuid::new_v4()), 32)
        .await
        .unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        32
    );

    store.set_max_agents(project.id, 16).await.unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        32,
        "default FLEET_MAX_AGENTS (16) must not drop Settings 32"
    );

    store.set_max_agents(project.id, 40).await.unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        40
    );

    store.set_max_agents(project.id, 0).await.unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        40,
        "clamp 0→1 must not lower an existing cap"
    );

    store.set_max_agents(project.id, 9999).await.unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        fleet_orchestrator::domain::MAX_PROJECT_AGENTS
    );
}

#[tokio::test]
async fn update_fleet_settings_lowers_after_set_max_agents_raise() {
    let store = setup().await;
    let project = store
        .create_project(&format!("set-after-raise-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();

    store.set_max_agents(project.id, 16).await.unwrap();
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        16
    );

    let cfg = store
        .update_fleet_settings(project.id, 4, "off", "")
        .await
        .unwrap();
    assert_eq!(cfg.max_agents, 4, "Settings SET must lower after a GREATEST raise");
    assert_eq!(
        store.get_project_config(project.id).await.unwrap().max_agents,
        4
    );
}

#[tokio::test]
async fn live_events_persist_and_duplicate_seq_is_noop() {
    let store = setup().await;
    let (_project_id, session_id, _task_id) = inbox_session(&store).await;

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
    assert_eq!(n2, 0, "duplicate (session_id, report_seq, idx) is a no-op");

    let live = store.list_live_events(session_id, 50).await.unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0]["text"], "built calculator");
    assert_eq!(live[0]["kind"], "update");
}

#[tokio::test]
async fn live_events_same_seq_two_idx_activity_keys_unique() {
    let store = setup().await;
    let (_project_id, session_id, _task_id) = inbox_session(&store).await;

    let ts = chrono::Utc::now();
    let n = store
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
    assert_eq!(n, 2, "same report_seq extra idx still inserts");

    let act = store
        .list_recent_live_activity(&[session_id], 40)
        .await
        .unwrap();
    assert_eq!(act.len(), 2);
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
async fn list_live_events_returns_newest_window_in_seq_order() {
    let store = setup().await;
    let (_project_id, session_id, _task_id) = inbox_session(&store).await;

    let ts = chrono::Utc::now();
    for seq in 1..=5 {
        let n = store
            .insert_live_events(
                session_id,
                seq,
                &[(ts, "update".into(), format!("seq-{seq}"))],
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    let live = store.list_live_events(session_id, 2).await.unwrap();
    assert_eq!(live.len(), 2);
    assert_eq!(live[0]["text"], "seq-4");
    assert_eq!(live[1]["text"], "seq-5");
}

#[tokio::test]
async fn list_recent_live_activity_only_requested_sessions() {
    let store = setup().await;
    let (_project_a, session_a, _task_a) = inbox_session(&store).await;
    let (_project_b, session_b, _task_b) = inbox_session(&store).await;

    let ts = chrono::Utc::now();
    store
        .insert_live_events(session_a, 1, &[(ts, "update".into(), "from-a".into())])
        .await
        .unwrap();
    store
        .insert_live_events(session_b, 1, &[(ts, "update".into(), "from-b".into())])
        .await
        .unwrap();

    let act = store
        .list_recent_live_activity(&[session_a], 40)
        .await
        .unwrap();
    assert_eq!(act.len(), 1);
    assert_eq!(act[0]["session_id"], serde_json::json!(session_a));
    assert_eq!(act[0]["text"], "from-a");
    assert!(
        act.iter()
            .all(|e| e["session_id"] != serde_json::json!(session_b)),
        "dashboard activity must not include unrequested sessions"
    );
}

/// Materialized t1 forced ready for begin_spawn_assignment.
async fn ready_t1(store: &Store, prefix: &str) -> (Uuid, Uuid, Uuid) {
    let project = store
        .create_project(&format!("{prefix}-{}", Uuid::new_v4()), 2)
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
    (project.id, run_id, task.id)
}

/// First session exited worker_replaced after progress=40; second spawning holds the claim.
async fn two_attempt_replace_board(
    store: &Store,
) -> (Uuid, Uuid, SpawnAssignment, SpawnAssignment) {
    let (project_id, run_id, task_id) = ready_t1(store, "repl").await;
    let first = store
        .begin_spawn_assignment(
            task_id,
            project_id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/a"),
        )
        .await
        .unwrap()
        .expect("first spawn");
    store
        .apply_heartbeat(first.session_id, task_id, 1, Some(40))
        .await
        .unwrap();
    store
        .retire_live_sessions(task_id, None, "worker_replaced")
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(task_id)
        .execute(store.pool())
        .await
        .unwrap();
    let second = store
        .begin_spawn_assignment(
            task_id,
            project_id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/b"),
        )
        .await
        .unwrap()
        .expect("second spawn");
    (project_id, task_id, first, second)
}

#[tokio::test]
async fn complete_spawn_failure_does_not_overwrite_retired_worker_replaced() {
    let store = setup().await;
    let (project_id, run_id, task_id) = ready_t1(&store, "repl-fail").await;
    let first = store
        .begin_spawn_assignment(
            task_id,
            project_id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/a"),
        )
        .await
        .unwrap()
        .expect("first spawn");

    let retired = store
        .retire_live_sessions(task_id, None, "worker_replaced")
        .await
        .unwrap();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].id, first.session_id);
    assert_eq!(retired[0].status, "exited");
    assert_eq!(retired[0].exit_reason.as_deref(), Some("worker_replaced"));

    assert!(
        !store
            .complete_spawn_failure(first.session_id)
            .await
            .unwrap(),
        "late spawn-failed must not overwrite exited/worker_replaced"
    );
    let old = store.get_session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, "exited");
    assert_eq!(old.exit_reason.as_deref(), Some("worker_replaced"));
}

#[tokio::test]
async fn retire_live_sessions_keep_new_skips_already_exited() {
    let store = setup().await;
    let (_project_id, task_id, first, second) = two_attempt_replace_board(&store).await;

    let retired2 = store
        .retire_live_sessions(task_id, Some(second.session_id), "superseded")
        .await
        .unwrap();
    assert!(retired2.is_empty(), "first session already exited");

    let old = store.get_session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, "exited");
    assert_eq!(old.exit_reason.as_deref(), Some("worker_replaced"));
    let live = store.get_session(second.session_id).await.unwrap().unwrap();
    assert_eq!(live.status, "spawning");
}

#[tokio::test]
async fn replace_does_not_leave_sibling_or_accept_dead_heartbeat() {
    let store = setup().await;
    let (project_id, task_id, first, second) = two_attempt_replace_board(&store).await;
    let before = store.get_session(first.session_id).await.unwrap().unwrap();
    let hb_at = before.last_heartbeat_at;
    assert_eq!(before.progress_pct, 40);
    assert_eq!(before.status, "exited");

    store
        .apply_heartbeat(first.session_id, task_id, 9, Some(99))
        .await
        .unwrap();

    let old = store.get_session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, "exited");
    assert_eq!(old.exit_reason.as_deref(), Some("worker_replaced"));
    assert_eq!(old.progress_pct, 40);
    assert_eq!(old.last_heartbeat_at, hb_at);

    let task_now = store.get_task(project_id, task_id).await.unwrap().unwrap();
    assert_eq!(task_now.claimed_session_id, Some(second.session_id));
    assert_eq!(task_now.progress_pct, 40);
    assert_eq!(task_now.last_report_seq, 1);
}

#[tokio::test]
async fn apply_heartbeat_promotes_spawning_to_healthy_and_stamps_progress() {
    let store = setup().await;
    let (project_id, run_id, task_id) = ready_t1(&store, "hb-promote").await;
    let assignment = store
        .begin_spawn_assignment(task_id, project_id, run_id, "coding", "remote", None)
        .await
        .unwrap()
        .expect("spawn");
    let session_id = assignment.session_id;

    let before = store.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(before.status, "spawning");
    assert_eq!(before.progress_pct, 0);
    assert!(before.last_heartbeat_at.is_none());

    store
        .apply_heartbeat(session_id, task_id, 1, Some(5))
        .await
        .unwrap();

    let after = store.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(after.status, "healthy");
    assert_eq!(after.progress_pct, 5);
    assert!(after.last_heartbeat_at.is_some());
}

#[tokio::test]
async fn replace_session_already_dead_does_not_reopen_while_sibling_live() {
    let store = setup().await;
    let (project_id, task_id, first, second) = two_attempt_replace_board(&store).await;
    let tick = TickRunner::with_store(store.clone());

    let report = tick
        .replace_session(project_id, first.session_id)
        .await
        .unwrap();
    assert!(report.already_dead);
    assert!(!report.replaced);
    assert_eq!(report.task_id, task_id);

    let task_now = store.get_task(project_id, task_id).await.unwrap().unwrap();
    assert_eq!(task_now.claimed_session_id, Some(second.session_id));
    assert_ne!(
        task_now.status, "ready",
        "already-dead replace must not unclaim a live sibling"
    );
}

#[tokio::test]
async fn apply_retry_after_failure_noops_when_live_claim_is_other_attempt() {
    let store = setup().await;
    let (project_id, task_id, first, second) = two_attempt_replace_board(&store).await;

    apply_retry_after_failure(&store, task_id, first.attempt, 0, false, ExitReason::Stale)
        .await
        .unwrap();

    let task_now = store.get_task(project_id, task_id).await.unwrap().unwrap();
    assert_eq!(task_now.claimed_session_id, Some(second.session_id));
    assert!(
        matches!(task_now.status.as_str(), "assigned" | "in_progress"),
        "stale retry of first attempt must not bounce the live claim: {}",
        task_now.status
    );
}

#[tokio::test]
async fn authenticate_worker_session_none_after_retire_exited() {
    let store = setup().await;
    let (project_id, run_id, task_id) = ready_t1(&store, "repl-auth").await;
    let first = store
        .begin_spawn_assignment(
            task_id,
            project_id,
            run_id,
            "coding",
            "remote",
            Some("/tmp/a"),
        )
        .await
        .unwrap()
        .expect("first spawn");
    store
        .retire_live_sessions(task_id, None, "worker_replaced")
        .await
        .unwrap();

    let authed = store
        .authenticate_worker_session(first.session_id, &first.worker_token)
        .await
        .unwrap();
    assert!(
        authed.is_none(),
        "exited/worker_replaced session must not authenticate"
    );
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

fn independent_plan(project_id: Uuid, ids: &[&str]) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: ids
            .iter()
            .enumerate()
            .map(|(i, id)| PlanTask {
                plan_task_id: (*id).into(),
                title: (*id).into(),
                summary: String::new(),
                spec_md: String::new(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 100 - i as i32,
                external_ref: None,
                timeout_s: None,
                max_retries: Some(1),
            })
            .collect(),
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::Leave,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

async fn ready_assign_session(
    store: &Store,
    project_id: Uuid,
    run_id: Uuid,
    task_id: Uuid,
) -> Uuid {
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(task_id)
        .execute(store.pool())
        .await
        .unwrap();
    let session_id = Uuid::new_v4();
    assert!(store.cas_assign_task(task_id, session_id, 1).await.unwrap());
    store
        .insert_session_spawning(&fleet_orchestrator::store::NewWorkerSession {
            id: session_id,
            project_id,
            run_id,
            board_task_id: task_id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: format!("tok-{session_id}"),
            worker_profile: "coding".into(),
            work_dir: Some(format!("/tmp/fleet-cas-{session_id}")),
        })
        .await
        .unwrap();
    session_id
}

#[tokio::test]
async fn apply_task_terminal_cas_leaves_blocked_hitl_and_stale_claim_untouched() {
    let store = setup().await;
    let project = store
        .create_project(&format!("cas-term-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = independent_plan(project.id, &["blocked", "stale", "ok"]);
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let blocked_id = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "blocked")
        .unwrap()
        .id;
    let stale_id = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "stale")
        .unwrap()
        .id;
    let ok_id = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "ok")
        .unwrap()
        .id;

    let blocked_claim = ready_assign_session(&store, project.id, run_id, blocked_id).await;
    store
        .apply_task_terminal(blocked_id, blocked_claim, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();
    store
        .apply_task_terminal(blocked_id, blocked_claim, "done_success", None, 2)
        .await
        .unwrap();
    store
        .apply_task_terminal(blocked_id, blocked_claim, "done_failed", Some("nope"), 3)
        .await
        .unwrap();
    store
        .apply_task_terminal(blocked_id, blocked_claim, "blocked", Some("other"), 4)
        .await
        .unwrap();

    let stale_claim = ready_assign_session(&store, project.id, run_id, stale_id).await;
    store
        .apply_task_terminal(stale_id, Uuid::new_v4(), "done_success", None, 2)
        .await
        .unwrap();

    let ok_claim = ready_assign_session(&store, project.id, run_id, ok_id).await;
    store
        .apply_task_terminal(ok_id, ok_claim, "done_success", None, 2)
        .await
        .unwrap();

    let blocked = store.get_task(project.id, blocked_id).await.unwrap().unwrap();
    assert_eq!(blocked.status, "blocked");
    assert_eq!(blocked.claimed_session_id, Some(blocked_claim));
    assert_eq!(blocked.block_reason.as_deref(), Some("needs_approval"));

    let stale = store.get_task(project.id, stale_id).await.unwrap().unwrap();
    assert_eq!(stale.status, "assigned");
    assert_eq!(stale.claimed_session_id, Some(stale_claim));

    let ok = store.get_task(project.id, ok_id).await.unwrap().unwrap();
    assert_eq!(ok.status, "done_success");
    assert_eq!(ok.claimed_session_id, None);
}

async fn plant_blocked_with_hitl_intent(
    store: &Store,
    name: &str,
    notes: &str,
) -> (Uuid, Uuid, Uuid) {
    let project = store
        .create_project(&format!("{name}-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = independent_plan(project.id, &["t1"]);
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let task_id = store
        .load_board_snapshot(project.id)
        .await
        .unwrap()
        .tasks
        .into_iter()
        .find(|t| t.plan_task_id == "t1")
        .unwrap()
        .id;
    let claim = ready_assign_session(store, project.id, run_id, task_id).await;
    store
        .apply_task_terminal(task_id, claim, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();
    assert!(
        store
            .request_hitl_resume(project.id, task_id, notes)
            .await
            .unwrap(),
        "blocked task must accept HITL resume"
    );
    (project.id, run_id, task_id)
}

#[tokio::test]
async fn apply_hitl_resume_skips_when_run_cancel_requested() {
    let store = setup().await;
    let (project_id, run_id, task_id) =
        plant_blocked_with_hitl_intent(&store, "hitl-cancel", "approved by operator").await;
    store.request_cancel(project_id, run_id).await.unwrap();

    let rows = store.apply_hitl_resume(project_id).await.unwrap();
    assert_eq!(rows, 0, "K21 cancel wins over HITL resume");

    let task = store.get_task(project_id, task_id).await.unwrap().unwrap();
    assert_eq!(task.status, "blocked", "task={task:?}");
    assert!(
        task.hitl_resume_requested_at.is_some(),
        "skipped apply must leave intent, task={task:?}"
    );
    assert!(task.hitl_resolved_at.is_none(), "task={task:?}");
    assert_eq!(task.block_reason.as_deref(), Some("needs_approval"), "task={task:?}");
}

#[tokio::test]
async fn request_hitl_resume_false_unless_blocked() {
    let store = setup().await;
    let project = store
        .create_project(&format!("hitl-gate-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = independent_plan(
        project.id,
        &["assigned", "in_progress", "ready", "done_success", "blocked"],
    );
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let id_of = |name: &str| {
        snap.tasks
            .iter()
            .find(|t| t.plan_task_id == name)
            .unwrap()
            .id
    };
    let assigned_id = id_of("assigned");
    let in_progress_id = id_of("in_progress");
    let ready_id = id_of("ready");
    let done_id = id_of("done_success");
    let blocked_id = id_of("blocked");

    ready_assign_session(&store, project.id, run_id, assigned_id).await;
    ready_assign_session(&store, project.id, run_id, in_progress_id).await;
    sqlx::query("UPDATE board_tasks SET status = 'in_progress' WHERE id = $1")
        .bind(in_progress_id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(ready_id)
        .execute(store.pool())
        .await
        .unwrap();
    let done_claim = ready_assign_session(&store, project.id, run_id, done_id).await;
    store
        .apply_task_terminal(done_id, done_claim, "done_success", None, 1)
        .await
        .unwrap();
    let blocked_claim = ready_assign_session(&store, project.id, run_id, blocked_id).await;
    store
        .apply_task_terminal(blocked_id, blocked_claim, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();

    for (tid, want_status) in [
        (assigned_id, "assigned"),
        (in_progress_id, "in_progress"),
        (ready_id, "ready"),
        (done_id, "done_success"),
    ] {
        let ok = store
            .request_hitl_resume(project.id, tid, "nope")
            .await
            .unwrap();
        assert!(!ok, "{want_status} must not accept HITL resume");
        let task = store.get_task(project.id, tid).await.unwrap().unwrap();
        assert_eq!(task.status, want_status, "task={task:?}");
        assert!(task.hitl_notes.is_none(), "{want_status} task={task:?}");
        assert!(
            task.hitl_resume_requested_at.is_none(),
            "{want_status} task={task:?}"
        );
    }

    let ok = store
        .request_hitl_resume(project.id, blocked_id, "approved by operator")
        .await
        .unwrap();
    assert!(ok, "blocked must accept HITL resume");
    let blocked = store.get_task(project.id, blocked_id).await.unwrap().unwrap();
    assert_eq!(blocked.status, "blocked", "task={blocked:?}");
    assert_eq!(
        blocked.hitl_notes.as_deref(),
        Some("approved by operator"),
        "task={blocked:?}"
    );
    assert!(
        blocked.hitl_resume_requested_at.is_some(),
        "task={blocked:?}"
    );
}

#[tokio::test]
async fn apply_hitl_resume_sets_ready_clears_claim_reason_and_intent() {
    let store = setup().await;
    let (project_id, _run_id, task_id) =
        plant_blocked_with_hitl_intent(&store, "hitl-apply", "approved by operator").await;

    let rows = store.apply_hitl_resume(project_id).await.unwrap();
    assert_eq!(rows, 1);

    let task = store.get_task(project_id, task_id).await.unwrap().unwrap();
    assert_eq!(task.status, "ready", "task={task:?}");
    assert!(task.claimed_session_id.is_none(), "task={task:?}");
    assert!(task.block_reason.is_none(), "task={task:?}");
    assert!(task.hitl_resume_requested_at.is_none(), "task={task:?}");
    assert!(task.hitl_resolved_at.is_some(), "task={task:?}");
    assert_eq!(
        task.hitl_notes.as_deref(),
        Some("approved by operator"),
        "task={task:?}"
    );
}

#[tokio::test]
async fn recover_orphaned_active_tasks_ready_when_session_gone() {
    let store = setup().await;
    let project = store
        .create_project(&format!("orphan-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = independent_plan(project.id, &["gone", "live", "nullclaim"]);
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let id_of = |name: &str| {
        snap.tasks
            .iter()
            .find(|t| t.plan_task_id == name)
            .unwrap()
            .id
    };
    let gone_id = id_of("gone");
    let live_id = id_of("live");
    let null_id = id_of("nullclaim");

    let gone_sid = ready_assign_session(&store, project.id, run_id, gone_id).await;
    sqlx::query("UPDATE board_tasks SET status = 'in_progress' WHERE id = $1")
        .bind(gone_id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM worker_sessions WHERE id = $1")
        .bind(gone_sid)
        .execute(store.pool())
        .await
        .unwrap();

    let live_sid = ready_assign_session(&store, project.id, run_id, live_id).await;

    sqlx::query(
        "UPDATE board_tasks SET status = 'assigned', claimed_session_id = NULL WHERE id = $1",
    )
    .bind(null_id)
    .execute(store.pool())
    .await
    .unwrap();

    let n = store
        .recover_orphaned_active_tasks(project.id)
        .await
        .unwrap();
    assert_eq!(n, 2, "gone + nullclaim must recover; live must stay");

    let gone = store.get_task(project.id, gone_id).await.unwrap().unwrap();
    assert_eq!(gone.status, "ready", "task={gone:?}");
    assert!(gone.claimed_session_id.is_none(), "task={gone:?}");
    assert_eq!(gone.failure_reason.as_deref(), Some("session_gone"));

    let live = store.get_task(project.id, live_id).await.unwrap().unwrap();
    assert_eq!(live.status, "assigned", "task={live:?}");
    assert_eq!(live.claimed_session_id, Some(live_sid));

    let null = store.get_task(project.id, null_id).await.unwrap().unwrap();
    assert_eq!(null.status, "ready", "task={null:?}");
    assert!(null.claimed_session_id.is_none(), "task={null:?}");
}
