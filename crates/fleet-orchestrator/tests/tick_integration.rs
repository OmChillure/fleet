//! Tick integration: materialize, deps, host assign, inbox, cancel, rollup.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_db::repos::workspaces;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
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
    TickRunner::new(
        store,
        std::env::temp_dir().join("fleet-orch-tick-int"),
        "http://192.0.2.10:8080",
    )
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
async fn tick_materializes_and_holds_ready_without_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("pin min_isolation=vm so a shared cluster host is not picked");
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
    let host_id = pair_online_host(&store, project.id).await;
    let expected_handle = format!("host:{host_id}");
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1, "r1={r1:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert!(
        matches!(t1.status.as_str(), "assigned" | "in_progress"),
        "t1={}",
        t1.status
    );
    assert_eq!(t2.status, "queued", "child stays queued until parent inbox");
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("t1 session");
    assert_eq!(session.runtime_backend, "remote");
    assert_eq!(session.runtime_handle.as_deref(), Some(expected_handle.as_str()));
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
    assert_eq!(r2.spawned, 1, "t2 should assign once: r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "done_success");
    let s2 = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t2.id)
        .expect("t2 session");
    assert_eq!(s2.runtime_backend, "remote");
    assert_eq!(s2.runtime_handle.as_deref(), Some(expected_handle.as_str()));
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

    let r3 = tick.tick_project(project.id).await.unwrap();
    assert!(r3.runs_terminated >= 1, "r3={r3:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "done_success");
    assert_eq!(t2.status, "done_success");
    for task in [t1, t2] {
        let session = snap
            .sessions
            .iter()
            .find(|s| s.board_task_id == task.id)
            .expect("session");
        assert_eq!(
            session.runtime_handle.as_deref(),
            Some(expected_handle.as_str())
        );
    }

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
async fn tick_skips_smuggled_draft_plan() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-draft-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    let body = chain_plan(project.id);
    insert_plan_with_status(&store, &body, WorkPlanStatus::Draft).await;

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.plans_materialized, 0);
    assert_eq!(r.tasks_materialized, 0);
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    assert!(snap.tasks.is_empty());
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
    assert_eq!(r2.spawned, 0, "cancelling run must not spawn t2: {r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    let t1_session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("t1 session");
    assert!(
        matches!(t1_session.status.as_str(), "killing" | "exited"),
        "t1 session={}",
        t1_session.status
    );
    assert_eq!(t1_session.exit_reason.as_deref(), Some("cancel"));
    assert_eq!(t2.status, "cancelled");
    assert!(
        snap.sessions.iter().all(|s| s.board_task_id != t2.id),
        "t2 must not have a session"
    );

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "cancelled");
}

#[tokio::test]
async fn tick_cancel_mid_run_enqueues_term_or_kill_claim_for_assigned_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-sig-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let host_id = pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1);

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t1_session_id = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("t1 session")
        .id;

    store.request_cancel(project.id, run_id).await.unwrap();
    tick.tick_project(project.id).await.unwrap();

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        r#"
        SELECT payload->>'session_id', payload->>'host_id', payload->>'signal'
        FROM work_claims
        WHERE workspace_id = $1 AND kind = 'signal_session'
        "#,
    )
    .bind(project.id)
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "signal_session rows={rows:?}");
    assert_eq!(rows[0].0, t1_session_id.to_string());
    assert_eq!(rows[0].1, host_id.to_string());
    assert!(
        matches!(rows[0].2.as_str(), "term" | "kill"),
        "signal={}",
        rows[0].2
    );
}

#[tokio::test]
async fn tick_cancel_after_all_done_success_rollups_cancelled() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-k27-{}", Uuid::new_v4()), 3)
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

    store.request_cancel(project.id, run_id).await.unwrap();
    runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let t2 = snap.tasks.iter().find(|t| t.plan_task_id == "t2").unwrap();
    assert_eq!(t1.status, "done_success");
    assert_eq!(t2.status, "done_success");

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "cancelled");
}

#[tokio::test]
async fn tick_cancel_positive_kill_grace_marks_killing_without_immediate_killed() {
    let store = setup().await;
    let project = store
        .create_project(&format!("tick-grace-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    pair_online_host(&store, project.id).await;
    let body = chain_plan(project.id);
    let run_id = body.run_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();

    sqlx::query("UPDATE project_configs SET kill_grace_s = 30 WHERE project_id = $1")
        .bind(project.id)
        .execute(store.pool())
        .await
        .unwrap();

    let tick = runner(store.clone());
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 1);

    store.request_cancel(project.id, run_id).await.unwrap();
    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r2.killed, 0, "grace holds kill r2={r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("t1 session");
    assert_eq!(session.status, "killing");
    assert_eq!(session.exit_reason.as_deref(), Some("cancel"));
    assert_ne!(t1.status, "cancelled");

    let row: (String,) = sqlx::query_as("SELECT status FROM work_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "active");
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
                timeout_s: Some(3600),
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
    attempt: i32,
    backend: &str,
) -> Uuid {
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(task_id)
        .execute(store.pool())
        .await
        .unwrap();
    let session_id = Uuid::new_v4();
    assert!(
        store
            .cas_assign_task(task_id, session_id, attempt)
            .await
            .unwrap()
    );
    store
        .insert_session_spawning(&fleet_orchestrator::store::NewWorkerSession {
            id: session_id,
            project_id,
            run_id,
            board_task_id: task_id,
            attempt,
            runtime_backend: backend.into(),
            worker_token: format!("tok-{session_id}"),
            worker_profile: "coding".into(),
            work_dir: Some(format!("/tmp/fleet-inbox-miss-{session_id}")),
        })
        .await
        .unwrap();
    session_id
}

#[tokio::test]
async fn apply_inbox_validation_bounce_or_exhausted_on_blocked_or_unclaimed_does_not_reopen() {
    let store = setup().await;
    let project = store
        .create_project(&format!("inbox-gate-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    let body = independent_plan(
        project.id,
        &[
            "block_bounce",
            "block_exhaust",
            "ready_bounce",
            "ready_exhaust",
        ],
    );
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let block_bounce = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "block_bounce")
        .unwrap()
        .id;
    let block_exhaust = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "block_exhaust")
        .unwrap()
        .id;
    let ready_bounce = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "ready_bounce")
        .unwrap()
        .id;
    let ready_exhaust = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "ready_exhaust")
        .unwrap()
        .id;

    let bb_sess = ready_assign_session(&store, project.id, run_id, block_bounce, 1, "remote").await;
    let be_sess = ready_assign_session(&store, project.id, run_id, block_exhaust, 1, "remote").await;
    let rb_sess = ready_assign_session(&store, project.id, run_id, ready_bounce, 1, "remote").await;
    let re_sess = ready_assign_session(&store, project.id, run_id, ready_exhaust, 1, "remote").await;

    store
        .apply_task_terminal(block_bounce, bb_sess, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();
    store
        .apply_task_terminal(block_exhaust, be_sess, "blocked", Some("needs_approval"), 1)
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(ready_bounce)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET status = 'ready', claimed_session_id = NULL WHERE id = $1")
        .bind(ready_exhaust)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET hitl_notes = $2 WHERE id = $1")
        .bind(block_exhaust)
        .bind("Task validation failed\nTask validation failed")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE board_tasks SET hitl_notes = $2 WHERE id = $1")
        .bind(ready_exhaust)
        .bind("Task validation failed\nTask validation failed")
        .execute(store.pool())
        .await
        .unwrap();

    store
        .append_worker_report(
            project.id,
            bb_sess,
            block_bounce,
            2,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();
    store
        .append_worker_report(
            project.id,
            be_sess,
            block_exhaust,
            2,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();
    store
        .append_worker_report(
            project.id,
            rb_sess,
            ready_bounce,
            2,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();
    store
        .append_worker_report(
            project.id,
            re_sess,
            ready_exhaust,
            2,
            "status",
            serde_json::json!({"status": "success"}),
        )
        .await
        .unwrap();

    // No paired host: tick applies inbox and must not spawn a replacement.
    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert!(r.inbox_applied >= 4, "r={r:?}");
    assert!(
        store
            .list_unapplied_reports(project.id)
            .await
            .unwrap()
            .is_empty()
    );

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let bb = snap
        .tasks
        .iter()
        .find(|t| t.id == block_bounce)
        .unwrap();
    assert_eq!(bb.status, "blocked");
    assert_eq!(bb.claimed_session_id, Some(bb_sess));
    assert_eq!(bb.block_reason.as_deref(), Some("needs_approval"));
    assert!(bb.hitl_notes.is_none());
    assert_ne!(bb.failure_reason.as_deref(), Some("validation_failed"));
    assert_ne!(
        bb.failure_reason.as_deref(),
        Some("validation_failed_exhausted")
    );

    let be = snap
        .tasks
        .iter()
        .find(|t| t.id == block_exhaust)
        .unwrap();
    assert_eq!(be.status, "blocked");
    assert_eq!(be.claimed_session_id, Some(be_sess));
    assert_eq!(be.block_reason.as_deref(), Some("needs_approval"));
    assert_eq!(
        be.hitl_notes.as_deref(),
        Some("Task validation failed\nTask validation failed")
    );
    assert_ne!(
        be.failure_reason.as_deref(),
        Some("validation_failed_exhausted")
    );

    let rb = snap
        .tasks
        .iter()
        .find(|t| t.id == ready_bounce)
        .unwrap();
    assert_eq!(rb.status, "ready");
    assert_eq!(rb.claimed_session_id, None);
    assert!(rb.hitl_notes.is_none());
    assert_ne!(rb.failure_reason.as_deref(), Some("validation_failed"));

    let re = snap
        .tasks
        .iter()
        .find(|t| t.id == ready_exhaust)
        .unwrap();
    assert_eq!(re.status, "ready");
    assert_eq!(re.claimed_session_id, None);
    assert_eq!(
        re.hitl_notes.as_deref(),
        Some("Task validation failed\nTask validation failed")
    );
    assert_ne!(
        re.failure_reason.as_deref(),
        Some("validation_failed_exhausted")
    );
}

#[tokio::test]
async fn apply_inbox_stale_success_after_reclaim_does_not_complete_wrong_session() {
    let store = setup().await;
    let project = store
        .create_project(&format!("inbox-stale-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    let body = independent_plan(project.id, &["t1"]);
    let run_id = body.run_id;
    let plan_id = body.plan_id;
    store.insert_accepted_plan(project.id, body).await.unwrap();
    store.materialize_plan(project.id, plan_id).await.unwrap();
    let t1 = store
        .load_board_snapshot(project.id)
        .await
        .unwrap()
        .tasks
        .into_iter()
        .find(|t| t.plan_task_id == "t1")
        .unwrap()
        .id;

    let session_a = ready_assign_session(&store, project.id, run_id, t1, 1, "stub").await;
    let session_b = ready_assign_session(&store, project.id, run_id, t1, 2, "stub").await;
    store
        .apply_heartbeat(session_b, t1, 1, Some(40))
        .await
        .unwrap();

    store
        .append_worker_report(
            project.id,
            session_a,
            t1,
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

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert!(r.inbox_applied >= 1, "r={r:?}");
    assert!(
        store
            .list_unapplied_reports(project.id)
            .await
            .unwrap()
            .is_empty()
    );

    let t1 = store.get_task(project.id, t1).await.unwrap().unwrap();
    assert_eq!(t1.status, "in_progress");
    assert_eq!(t1.claimed_session_id, Some(session_b));
}
