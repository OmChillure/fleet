//! Tick must not local-spawn while a paired host is mid host-bin restart.
//! Kill mid-swap + auto_update on holds ready; no in-process fallback, no token rotate.
//! Offline without `software_updating` still leaves the task ready.
//! Requires DATABASE_URL.

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
    migrate(&pool).await.expect("orch migrate");
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    Store::new(pool)
}

fn runner(store: Store) -> TickRunner {
    TickRunner::with_store(store)
}

fn one_task_plan(project_id: Uuid) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: "Only".into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(0),
        }],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

async fn seed_workspace_for_project(store: &Store, project_id: Uuid) {
    let slug = format!("tu{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("tick-update")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(store: &Store, workspace_id: Uuid, name: &str) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, name.to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        name,
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("pair");
    host.id
}

async fn heartbeat_software(store: &Store, host_id: Uuid, updating: bool) {
    worker_hosts::heartbeat(
        store.pool(),
        host_id,
        true,
        false,
        Some("linux-x86_64"),
        Some("box"),
        None,
        0,
        Some(4),
        Some("abc"),
        None,
        Some(updating),
        None,
    )
    .await
    .expect("heartbeat");
}

async fn age_last_seen(store: &Store, host_id: Uuid, seconds: i64) {
    sqlx::query(&format!(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '{seconds} seconds' WHERE id = $1"
    ))
    .bind(host_id)
    .execute(store.pool())
    .await
    .expect("age last_seen");
}

#[tokio::test]
async fn tick_holds_ready_when_only_host_is_restarting() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-hold-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    let host_id = pair_host(&store, project.id, "restarting").await;
    heartbeat_software(&store, host_id, true).await;
    // Outside the 90s assignable window, inside the 2 min updating hold.
    age_last_seen(&store, host_id, 110).await;

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(
        r.spawned, 0,
        "must not local-spawn while host restarts: {r:?}"
    );
    assert_eq!(r.spawn_failed, 0, "{r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "ready",
        "ready must stay ready, not assigned to stub: {:?}",
        t1.status
    );
    assert!(
        snap.sessions.is_empty(),
        "no local session: {:?}",
        snap.sessions
    );
}

#[tokio::test]
async fn tick_offline_host_without_updating_leaves_ready() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-off-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    let host_id = pair_host(&store, project.id, "offline").await;
    heartbeat_software(&store, host_id, false).await;
    age_last_seen(&store, host_id, 110).await;

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(
        r.spawned, 0,
        "offline host without updating must not local-spawn: {r:?}"
    );

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "ready",
        "no assignable host → stay ready: {:?}",
        t1.status
    );
    assert!(
        snap.sessions.is_empty(),
        "no local session: {:?}",
        snap.sessions
    );
}

/// Guest killed mid host-bin swap (`software_updating`, last_seen stale but
/// inside the 2 min hold). `auto_update` on must not fail over to the
/// in-process fallback and must not rotate the token.
#[tokio::test]
async fn tick_holds_ready_when_guest_killed_mid_swap_auto_update_on() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-kill-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    let host_id = pair_host(&store, project.id, "mid-swap").await;
    heartbeat_software(&store, host_id, true).await;
    // Kill mid-swap: outside the 90s assignable window, inside the 2 min hold.
    age_last_seen(&store, host_id, 110).await;

    let before = worker_hosts::get(store.pool(), host_id)
        .await
        .expect("get")
        .expect("host");
    assert!(
        before.auto_update,
        "paired host defaults auto-update on (AGENTS.md lockout)"
    );
    let token_hash = before
        .machine_token_hash
        .clone()
        .expect("paired machine token");

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.spawned, 0, "must not local-spawn mid-swap: {r:?}");
    assert_eq!(r.spawn_failed, 0, "{r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "ready",
        "mid-swap hold_ready must keep the task ready: {:?}",
        t1.status
    );
    assert!(
        snap.sessions.is_empty(),
        "no local in-process session: {:?}",
        snap.sessions
    );

    let after = worker_hosts::get(store.pool(), host_id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        after.machine_token_hash.as_deref(),
        Some(token_hash.as_str()),
        "tick must not rotate machine token mid-swap (AGENTS.md lockout)"
    );
    assert!(after.auto_update, "auto_update stays on");
}
