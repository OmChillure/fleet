//! E2E matrix against the host-assign path (not an in-process plane runtime).
//!
//! Covers:
//! - fail-and-retry (stale kill → retry_wait → ready → success)
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
use fleet_orchestrator::orch::{TickReport, TickRunner};
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

fn assert_tick_report_zero(r: &TickReport, label: &str) {
    assert_eq!(r.plans_materialized, 0, "{label}={r:?}");
    assert_eq!(r.tasks_materialized, 0, "{label}={r:?}");
    assert_eq!(r.dep_updates, 0, "{label}={r:?}");
    assert_eq!(r.inbox_applied, 0, "{label}={r:?}");
    assert_eq!(r.spawned, 0, "{label}={r:?}");
    assert_eq!(r.spawn_failed, 0, "{label}={r:?}");
    assert_eq!(r.completed_via_runtime, 0, "{label}={r:?}");
    assert_eq!(r.killed, 0, "{label}={r:?}");
    assert_eq!(r.retried, 0, "{label}={r:?}");
    assert_eq!(r.reviews_bounced, 0, "{label}={r:?}");
    assert_eq!(r.cancel_tasks, 0, "{label}={r:?}");
    assert_eq!(r.retry_promoted, 0, "{label}={r:?}");
    assert_eq!(r.reaped, 0, "{label}={r:?}");
    assert_eq!(r.runs_terminated, 0, "{label}={r:?}");
    assert_eq!(r.external_synced, 0, "{label}={r:?}");
    assert_eq!(r.writebacks, 0, "{label}={r:?}");
}

async fn plant_kill_grace_s(store: &Store, project_id: Uuid, kill_grace_s: i32) {
    sqlx::query("UPDATE project_configs SET kill_grace_s = $2 WHERE project_id = $1")
        .bind(project_id)
        .bind(kill_grace_s)
        .execute(store.pool())
        .await
        .unwrap();
}

/// Inbox heartbeat so process_sessions sees Healthy (spawning uses spawn_timeout, not stale).
async fn apply_t1_heartbeat(store: &Store, project_id: Uuid) -> Uuid {
    let snap = store.load_board_snapshot(project_id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("session");
    store
        .append_worker_report(
            project_id,
            session.id,
            t1.id,
            session.attempt,
            "heartbeat",
            serde_json::json!({"progress_pct": 10}),
        )
        .await
        .unwrap();
    session.id
}

/// Default heartbeat_stale_s is 60; 120s past last_heartbeat_at is stale without sleeping.
async fn backdate_last_heartbeat(store: &Store, session_id: Uuid) {
    sqlx::query(
        "UPDATE worker_sessions SET last_heartbeat_at = now() - interval '120 seconds' WHERE id = $1",
    )
    .bind(session_id)
    .execute(store.pool())
    .await
    .unwrap();
}

async fn backdate_kill_grace_deadline(store: &Store, session_id: Uuid) {
    sqlx::query(
        "UPDATE worker_sessions SET kill_grace_deadline = now() - interval '1 second' WHERE id = $1",
    )
    .bind(session_id)
    .execute(store.pool())
    .await
    .unwrap();
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
    plant_kill_grace_s(&store, project.id, 0).await;

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let session_id = apply_t1_heartbeat(&store, project.id).await;
    let r_hb = tick.tick_project(project.id).await.unwrap();
    assert!(r_hb.inbox_applied >= 1, "r_hb={r_hb:?}");
    backdate_last_heartbeat(&store, session_id).await;

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r2.killed, 1, "stale kill r2={r2:?}");
    assert_eq!(r2.retried, 1, "stale retry r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "retry_wait",
        "after stale should be retry_wait, got {t1:?}"
    );
    assert_eq!(t1.attempt, 1, "attempt before re-spawn={}", t1.attempt);

    // apply_retry_after_failure hardcodes now+10s (ignores retry_backoff_s).
    // Advance the same clock promote_retry_wait takes; do not SQL-patch the column.
    let promoted = store
        .promote_retry_wait(project.id, Utc::now() + ChronoDuration::seconds(30))
        .await
        .unwrap();
    assert_eq!(promoted, 1, "clock-advanced promote");

    let r3 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r3.spawned, 1, "r3={r3:?}");
    report_success(&store, project.id, "t1").await;
    let r4 = tick.tick_project(project.id).await.unwrap();
    assert!(r4.inbox_applied >= 1, "r4={r4:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_success", "task={t1:?}");
    assert_eq!(t1.attempt, 2, "attempt={}", t1.attempt);
    assert_eq!(run_status(&store, run_id).await, "completed");
}

#[tokio::test]
async fn stale_kill_max_retries_0_marks_done_failed_and_fails_run() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-retry0-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = single_task(project.id, 0);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    plant_kill_grace_s(&store, project.id, 0).await;

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let session_id = apply_t1_heartbeat(&store, project.id).await;
    tick.tick_project(project.id).await.unwrap();
    backdate_last_heartbeat(&store, session_id).await;

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r2.killed, 1, "stale kill r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "done_failed", "task={t1:?}");
    assert_eq!(
        t1.failure_reason.as_deref(),
        Some("max_retries_exceeded"),
        "task={t1:?}"
    );
    assert_eq!(t1.attempt, 1, "attempt={}", t1.attempt);
    assert_eq!(run_status(&store, run_id).await, "failed");
}

#[tokio::test]
async fn retry_wait_not_promoted_while_retry_not_before_future() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-retry-gate-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = single_task(project.id, 1);
    store.insert_accepted_plan(project.id, body).await.unwrap();
    plant_kill_grace_s(&store, project.id, 0).await;

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let session_id = apply_t1_heartbeat(&store, project.id).await;
    tick.tick_project(project.id).await.unwrap();
    backdate_last_heartbeat(&store, session_id).await;

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r2.killed, 1, "stale kill r2={r2:?}");
    assert_eq!(r2.retried, 1, "stale retry r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "retry_wait", "task={t1:?}");

    let r3 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r3.retry_promoted, 0, "r3={r3:?}");
    assert_eq!(r3.spawned, 0, "r3={r3:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "retry_wait", "task={t1:?}");
}

#[tokio::test]
async fn positive_kill_grace_retries_only_after_deadline() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-grace-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = single_task(project.id, 1);
    store.insert_accepted_plan(project.id, body).await.unwrap();
    plant_kill_grace_s(&store, project.id, 30).await;

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let session_id = apply_t1_heartbeat(&store, project.id).await;
    tick.tick_project(project.id).await.unwrap();
    backdate_last_heartbeat(&store, session_id).await;

    let r_stale = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r_stale.killed, 0, "grace holds kill r_stale={r_stale:?}");
    assert_eq!(r_stale.retried, 0, "grace holds retry r_stale={r_stale:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.id == session_id)
        .expect("session");
    assert_eq!(session.status, "killing", "session={session:?}");
    assert_eq!(session.exit_reason.as_deref(), Some("stale"));
    assert_eq!(t1.status, "in_progress", "task={t1:?}");

    backdate_kill_grace_deadline(&store, session_id).await;
    let r_grace = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r_grace.retried, 1, "r_grace={r_grace:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "retry_wait", "task={t1:?}");
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
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("session");
    let blocked_session_id = session.id;

    store
        .append_worker_report(
            project.id,
            session.id,
            t1.id,
            session.attempt,
            "status",
            serde_json::json!({"status": "blocked", "reason": "needs_approval"}),
        )
        .await
        .unwrap();

    let r_block = tick.tick_project(project.id).await.unwrap();
    assert!(r_block.inbox_applied >= 1, "r_block={r_block:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "blocked", "task={t1:?}");
    assert_eq!(
        t1.block_reason.as_deref(),
        Some("needs_approval"),
        "task={t1:?}"
    );
    assert_eq!(t1.claimed_session_id, Some(blocked_session_id), "task={t1:?}");
    assert!(t1.hitl_notes.is_none(), "task={t1:?}");
    assert!(t1.hitl_resume_requested_at.is_none(), "task={t1:?}");
    assert_eq!(run_status(&store, run_id).await, "active");

    let ok = store
        .request_hitl_resume(project.id, t1.id, "approved by operator")
        .await
        .unwrap();
    assert!(ok, "resume intent should stick while blocked");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "blocked", "intent must not flip status, task={t1:?}");
    assert_eq!(
        t1.hitl_notes.as_deref(),
        Some("approved by operator"),
        "task={t1:?}"
    );
    assert!(
        t1.hitl_resume_requested_at.is_some(),
        "task={t1:?}"
    );
    assert_eq!(run_status(&store, run_id).await, "active");

    let r_resume = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r_resume.spawned, 1, "r_resume={r_resume:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "assigned", "Step 2b ready then spawn, task={t1:?}");
    assert!(t1.claimed_session_id.is_some(), "task={t1:?}");
    assert_ne!(
        t1.claimed_session_id,
        Some(blocked_session_id),
        "task={t1:?}"
    );
    assert!(t1.block_reason.is_none(), "task={t1:?}");
    assert!(t1.hitl_resume_requested_at.is_none(), "task={t1:?}");
    assert!(t1.hitl_resolved_at.is_some(), "task={t1:?}");
    assert_eq!(
        t1.hitl_notes.as_deref(),
        Some("approved by operator"),
        "task={t1:?}"
    );
    assert_eq!(run_status(&store, run_id).await, "active");
}

#[tokio::test]
async fn e2e_double_force_tick_idempotent() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-idemp-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    sqlx::query(
        r#"
        UPDATE board_tasks
        SET status = 'done_success', terminal_at = now(), updated_at = now()
        WHERE project_id = $1
        "#,
    )
    .bind(project.id)
    .execute(store.pool())
    .await
    .unwrap();
    store.set_run_terminal(run_id, "completed").await.unwrap();

    let before = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = before.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = before.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "done_success");
    assert_eq!(t2.status, "done_success");
    let t1_id = t1.id;
    let t2_id = t2.id;
    let sessions_before: Vec<Uuid> = before.sessions.iter().map(|s| s.id).collect();

    let tick = runner(store.clone());
    let r_a = tick.tick_project(project.id).await.unwrap();
    let r_b = tick.tick_project(project.id).await.unwrap();
    assert_tick_report_zero(&r_a, "r_a");
    assert_tick_report_zero(&r_b, "r_b");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.tasks.len(), 2);
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.id, t1_id);
    assert_eq!(t2.id, t2_id);
    assert_eq!(t1.status, "done_success");
    assert_eq!(t2.status, "done_success");
    let sessions_after: Vec<Uuid> = snap.sessions.iter().map(|s| s.id).collect();
    assert_eq!(sessions_after, sessions_before);
    assert_eq!(run_status(&store, run_id).await, "completed");
}

#[tokio::test]
async fn tick_second_call_while_in_progress_does_not_double_spawn() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-midtick-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    store
        .insert_accepted_plan(project.id, single_task(project.id, 0))
        .await
        .unwrap();

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.sessions.len(), 1);
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let claimed = t1.claimed_session_id.expect("claimed after spawn");

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r2.spawned, 0, "r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert_eq!(snap.sessions.len(), 1);
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.claimed_session_id, Some(claimed));
}

#[tokio::test]
async fn tick_lock_conflict_returns_zero_report() {
    let store = setup().await;
    let project = store
        .create_project(&format!("e2e-lock-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let mut held = store.lock_project(project.id).await.unwrap();
    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_tick_report_zero(&r, "lock");
    Store::unlock_project(&mut held, project.id).await.unwrap();
}
