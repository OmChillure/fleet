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

/// Docs §15.4: `draft` / `ready` / `accepted` / `superseded`.
/// A draft (or ready/superseded) plan on disk must not create board tasks.
/// Direct `materialize_plan` is the write path; list must not offer them.
#[tokio::test]
async fn draft_ready_superseded_plans_do_not_materialize() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plan-status-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();

    let mut withheld = Vec::new();
    for status in [
        WorkPlanStatus::Draft,
        WorkPlanStatus::Ready,
        WorkPlanStatus::Superseded,
    ] {
        let body = sample_body(project.id);
        let plan_id = body.plan_id;
        let run_id = body.run_id;
        store.insert_accepted_plan(project.id, body).await.unwrap();
        sqlx::query("UPDATE work_plans SET status = $2, accepted_at = NULL WHERE id = $1")
            .bind(plan_id)
            .bind(status.as_db())
            .execute(store.pool())
            .await
            .unwrap();
        withheld.push((plan_id, run_id, status));
    }

    let accepted = sample_body(project.id);
    let accepted_id = accepted.plan_id;
    store
        .insert_accepted_plan(project.id, accepted)
        .await
        .unwrap();

    let pending = store
        .list_pending_materialize_plans(project.id)
        .await
        .unwrap();
    assert_eq!(
        pending.iter().map(|p| p.plan_id).collect::<Vec<_>>(),
        vec![accepted_id],
        "list_pending must offer only the accepted plan, got {pending:?}"
    );
    assert_eq!(pending[0].plan_status, WorkPlanStatus::Accepted.as_db());

    for (plan_id, run_id, status) in &withheld {
        let n = store.materialize_plan(project.id, *plan_id).await.unwrap();
        assert_eq!(n, 0, "{status:?} plan must not materialize");

        let (got_status, materialized_at, run_status): (
            String,
            Option<chrono::DateTime<chrono::Utc>>,
            String,
        ) = sqlx::query_as(
            r#"
            SELECT p.status, p.materialized_at, r.status
            FROM work_plans p
            JOIN work_runs r ON r.id = p.run_id
            WHERE p.id = $1
            "#,
        )
        .bind(plan_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(got_status, status.as_db());
        assert!(
            materialized_at.is_none(),
            "{status:?} must not stamp materialized_at"
        );
        assert_eq!(
            run_status, "accepted",
            "{status:?} must not promote run {run_id}"
        );
    }

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(
        snap.tasks.is_empty(),
        "draft/ready/superseded must not create board tasks: {:?}",
        snap.tasks
            .iter()
            .map(|t| (t.plan_id, &t.plan_task_id, &t.status))
            .collect::<Vec<_>>()
    );

    let n = store
        .materialize_plan(project.id, accepted_id)
        .await
        .unwrap();
    assert_eq!(n, 2, "accepted plan must still materialize");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);
    assert!(
        snap.tasks.iter().all(|t| t.plan_id == accepted_id),
        "board must contain only the accepted plan's tasks: {:?}",
        snap.tasks.iter().map(|t| t.plan_id).collect::<Vec<_>>()
    );
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

async fn claimed_assigned_task(store: &Store) -> (Uuid, Uuid, Uuid, Uuid) {
    let project = store
        .create_project(&format!("term-{}", Uuid::new_v4()), 2)
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
    assert!(
        store.cas_assign_task(task.id, session_id, 1).await.unwrap(),
        "ready → assigned CAS"
    );
    use fleet_orchestrator::store::NewWorkerSession;
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id: project.id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: format!("tok-{session_id}"),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/x".into()),
        })
        .await
        .unwrap();
    (project.id, task.id, session_id, run_id)
}

async fn load_task(
    store: &Store,
    project_id: Uuid,
    task_id: Uuid,
) -> fleet_orchestrator::store::BoardTaskRow {
    store
        .get_task(project_id, task_id)
        .await
        .unwrap()
        .expect("task")
}

/// apply_task_terminal is only from assigned/in_progress with a matching
/// claimed_session_id. Blocked HITL must not be overwritten. A stale success
/// after bounce must not complete the wrong claim.
#[tokio::test]
async fn apply_task_terminal_only_from_assigned_in_progress_with_matching_claimed_session_id() {
    let store = setup().await;

    let (project_id, task_id, session_id, _) = claimed_assigned_task(&store).await;
    store
        .apply_task_terminal(task_id, session_id, "done_success", None, 1)
        .await
        .unwrap();
    let row = load_task(&store, project_id, task_id).await;
    assert_eq!(row.status, "done_success");
    assert!(
        row.claimed_session_id.is_none(),
        "success must clear the claim"
    );

    let (project_id, task_id, session_id, _) = claimed_assigned_task(&store).await;
    store
        .complete_spawn_success(
            session_id,
            task_id,
            load_task(&store, project_id, task_id).await.run_id,
            "h",
            None,
        )
        .await
        .unwrap();
    let row = load_task(&store, project_id, task_id).await;
    assert_eq!(row.status, "in_progress");
    store
        .apply_task_terminal(task_id, session_id, "done_failed", Some("worker_failed"), 2)
        .await
        .unwrap();
    let row = load_task(&store, project_id, task_id).await;
    assert_eq!(row.status, "done_failed");
    assert_eq!(row.failure_reason.as_deref(), Some("worker_failed"));

    // Blocked HITL must not be overwritten — not even by the claiming session.
    let (project_id, task_id, session_id, _) = claimed_assigned_task(&store).await;
    store
        .apply_task_terminal(task_id, session_id, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();
    sqlx::query(
        r#"
        UPDATE board_tasks
        SET hitl_notes = $2, hitl_notified_at = now(), updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(task_id)
    .bind("waiting on human")
    .execute(store.pool())
    .await
    .unwrap();
    let blocked = load_task(&store, project_id, task_id).await;
    assert_eq!(blocked.status, "blocked");
    assert_eq!(blocked.block_reason.as_deref(), Some("needs_approval"));
    assert_eq!(blocked.hitl_notes.as_deref(), Some("waiting on human"));
    assert_eq!(blocked.claimed_session_id, Some(session_id));

    store
        .apply_task_terminal(task_id, session_id, "done_success", None, 2)
        .await
        .unwrap();
    store
        .apply_task_terminal(task_id, session_id, "done_failed", Some("stale_fail"), 3)
        .await
        .unwrap();
    store
        .apply_task_terminal(task_id, session_id, "blocked", Some("overwrite_hitl"), 4)
        .await
        .unwrap();
    let still = load_task(&store, project_id, task_id).await;
    assert_eq!(
        still.status, "blocked",
        "blocked HITL must not be overwritten by a later terminal apply"
    );
    assert_eq!(
        still.block_reason.as_deref(),
        Some("needs_approval"),
        "blocked HITL reason must stay until resume"
    );
    assert_eq!(
        still.hitl_notes.as_deref(),
        Some("waiting on human"),
        "blocked HITL notes must stay"
    );
    assert_eq!(still.claimed_session_id, Some(session_id));
    assert!(still.terminal_at.is_none(), "HITL is not terminal");

    // Bounce clears the claim; a later session owns the card.
    let (project_id, task_id, stale, run_id) = claimed_assigned_task(&store).await;
    store
        .reopen_task_for_changes(task_id, "validation_failed", "bounce notes")
        .await
        .unwrap();
    store
        .free_session_claim(task_id, stale, "validation_failed")
        .await
        .unwrap();
    let bounced = load_task(&store, project_id, task_id).await;
    assert_eq!(bounced.status, "ready");
    assert!(bounced.claimed_session_id.is_none());

    store
        .apply_task_terminal(task_id, stale, "done_success", None, 9)
        .await
        .unwrap();
    let after_stale = load_task(&store, project_id, task_id).await;
    assert_eq!(
        after_stale.status, "ready",
        "stale success after bounce must not complete an unclaimed task"
    );
    assert!(after_stale.claimed_session_id.is_none());
    assert!(after_stale.terminal_at.is_none());

    let live = Uuid::new_v4();
    assert!(store.cas_assign_task(task_id, live, 2).await.unwrap());
    use fleet_orchestrator::store::NewWorkerSession;
    store
        .insert_session_spawning(&NewWorkerSession {
            id: live,
            project_id,
            run_id,
            board_task_id: task_id,
            attempt: 2,
            runtime_backend: "stub".into(),
            worker_token: format!("tok-{live}"),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/y".into()),
        })
        .await
        .unwrap();

    store
        .apply_task_terminal(task_id, stale, "done_success", None, 10)
        .await
        .unwrap();
    let reclaimed = load_task(&store, project_id, task_id).await;
    assert_eq!(
        reclaimed.status, "assigned",
        "stale success after bounce must not complete the wrong claim"
    );
    assert_eq!(reclaimed.claimed_session_id, Some(live));
    assert!(reclaimed.terminal_at.is_none());

    store
        .apply_task_terminal(task_id, live, "done_success", None, 11)
        .await
        .unwrap();
    let done = load_task(&store, project_id, task_id).await;
    assert_eq!(done.status, "done_success");
    assert!(done.claimed_session_id.is_none());

    // Mismatched claim on an otherwise-live assigned card.
    let (project_id, task_id, session_id, _) = claimed_assigned_task(&store).await;
    store
        .apply_task_terminal(task_id, Uuid::new_v4(), "done_success", None, 1)
        .await
        .unwrap();
    let row = load_task(&store, project_id, task_id).await;
    assert_eq!(row.status, "assigned");
    assert_eq!(row.claimed_session_id, Some(session_id));
}

async fn materialized_root_task(store: &Store) -> (Uuid, Uuid, Uuid, String) {
    let project = store
        .create_project(&format!("ill-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = sample_body(project.id);
    let plan_id = body.plan_id;
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let task = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(task.status, "queued");
    (project.id, task.id, run_id, task.worker_profile.clone())
}

fn must_not_persist(got: Result<bool, impl std::fmt::Debug>, edge: &str) {
    assert!(
        matches!(got, Ok(false) | Err(_)),
        "{edge} must not persist (false or Err), got {got:?}"
    );
}

/// Docs §15.1: illegal board-task transitions are rejected in the store.
/// Generic persist (`update_task_status`) must not write done_success→ready
/// or cancelled→assigned. Assignment CAS stays ready-only.
#[tokio::test]
async fn illegal_task_transition_rejected_in_store() {
    let store = setup().await;

    // 1. done_success must not go back to ready via generic persist.
    let (project_id, task_id, session_id, _) = claimed_assigned_task(&store).await;
    store
        .apply_task_terminal(task_id, session_id, "done_success", None, 1)
        .await
        .unwrap();
    let done = load_task(&store, project_id, task_id).await;
    assert_eq!(done.status, "done_success");
    let terminal_at = done.terminal_at;
    assert!(
        terminal_at.is_some(),
        "apply_task_terminal stamps terminal_at"
    );

    must_not_persist(
        store
            .update_task_status(task_id, "done_success", "ready", None)
            .await,
        "done_success → ready",
    );
    let still = load_task(&store, project_id, task_id).await;
    assert_eq!(
        still.status, "done_success",
        "done_success must not become ready"
    );
    assert_eq!(
        still.terminal_at, terminal_at,
        "terminal_at must stay after rejected persist"
    );

    // 2. cancelled must not become assigned via persist or assignment CAS.
    let (project_id, task_id, run_id, profile) = materialized_root_task(&store).await;
    store.set_task_cancelled(task_id, "cancel").await.unwrap();
    let cancelled = load_task(&store, project_id, task_id).await;
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.claimed_session_id.is_none());

    must_not_persist(
        store
            .update_task_status(task_id, "cancelled", "assigned", None)
            .await,
        "cancelled → assigned",
    );
    assert!(
        !store
            .cas_assign_task(task_id, Uuid::new_v4(), 1)
            .await
            .unwrap(),
        "cas_assign_task must not assign a cancelled task"
    );
    assert!(
        store
            .begin_spawn_assignment(task_id, project_id, run_id, &profile, "stub", None)
            .await
            .unwrap()
            .is_none(),
        "begin_spawn_assignment must not assign a cancelled task"
    );
    let still = load_task(&store, project_id, task_id).await;
    assert_eq!(still.status, "cancelled");
    assert!(
        still.claimed_session_id.is_none(),
        "cancelled claimed_session_id stays null"
    );

    // 3. Control: legal queued→ready persist and ready assignment still work.
    let (project_id, cas_id, _, _) = materialized_root_task(&store).await;
    let ok = store
        .update_task_status(cas_id, "queued", "ready", None)
        .await
        .expect("queued → ready persist");
    assert!(ok, "queued → ready must persist");
    let ready = load_task(&store, project_id, cas_id).await;
    assert_eq!(ready.status, "ready");
    let session_id = Uuid::new_v4();
    assert!(
        store.cas_assign_task(cas_id, session_id, 1).await.unwrap(),
        "cas_assign_task on ready still assigns"
    );
    let assigned = load_task(&store, project_id, cas_id).await;
    assert_eq!(assigned.status, "assigned");
    assert_eq!(assigned.claimed_session_id, Some(session_id));

    let (project_id, spawn_id, run_id, profile) = materialized_root_task(&store).await;
    assert!(
        store
            .update_task_status(spawn_id, "queued", "ready", None)
            .await
            .expect("queued → ready persist"),
        "queued → ready must persist"
    );
    let spawned = store
        .begin_spawn_assignment(spawn_id, project_id, run_id, &profile, "stub", None)
        .await
        .unwrap()
        .expect("begin_spawn_assignment on ready still assigns");
    let assigned = load_task(&store, project_id, spawn_id).await;
    assert_eq!(assigned.status, "assigned");
    assert_eq!(assigned.claimed_session_id, Some(spawned.session_id));
}
