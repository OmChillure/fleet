//! Offline without `software_updating` still leaves the task ready.
//! Fresh heartbeat after that wait assigns the recovered host.
//! Updating host past HOST_UPDATE_HOLD_SECS with no peer stays ready.
//! Requires DATABASE_URL.

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
async fn tick_offline_host_without_updating_leaves_ready() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-off-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("pin min_isolation=vm so a shared cluster host is not picked");
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
    assert_eq!(
        r.spawn_failed, 0,
        "offline without updating must not fail the task: {r:?}"
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

/// Ready-wait after an offline updating=false host must assign on the next
/// tick once last_seen is fresh again.
#[tokio::test]
async fn tick_offline_host_then_fresh_heartbeat_assigns() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-back-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("pin min_isolation=vm so a shared cluster host is not picked");
    let host_id = pair_host(&store, project.id, "back").await;
    heartbeat_software(&store, host_id, false).await;
    age_last_seen(&store, host_id, 110).await;

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let tick = TickRunner::new(
        store.clone(),
        std::env::temp_dir().join("fleet-orch-data"),
        "http://192.0.2.10:8080",
    );
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(r1.spawned, 0, "offline wait: {r1:?}");
    assert_eq!(r1.spawn_failed, 0, "{r1:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "ready", "{:?}", t1.status);
    assert!(snap.sessions.is_empty(), "{:?}", snap.sessions);

    workspaces::set_min_isolation(store.pool(), project.id, "process")
        .await
        .expect("restore min_isolation=process so the recovered host is pickable");
    heartbeat_software(&store, host_id, false).await;

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert_eq!(
        r2.spawned, 1,
        "fresh heartbeat must assign the recovered host: {r2:?}"
    );
    assert_eq!(r2.spawn_failed, 0, "{r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    let session = snap
        .sessions
        .iter()
        .find(|s| s.board_task_id == t1.id)
        .expect("remote session after host is back");
    assert_eq!(session.runtime_backend, "remote");
    let handle = format!("host:{host_id}");
    assert_eq!(
        session.runtime_handle.as_deref(),
        Some(handle.as_str()),
        "session must be on the recovered host, not a leftover cluster row"
    );
}

/// Hold lift with no peer: updating=true last_seen past HOST_UPDATE_HOLD_SECS
/// and no assignable host → stay ready.
#[tokio::test]
async fn tick_updating_host_past_hold_with_no_peer_leaves_ready() {
    let store = setup().await;
    let project = store
        .create_project(&format!("upd-past-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("pin min_isolation=vm so a shared cluster host is not picked");
    let host_id = pair_host(&store, project.id, "past-hold").await;
    heartbeat_software(&store, host_id, true).await;
    age_last_seen(&store, host_id, 130).await;

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
        "hold lift without a peer must not local-spawn: {r:?}"
    );
    assert_eq!(r.spawn_failed, 0, "{r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "ready",
        "no peer after hold lift → stay ready: {:?}",
        t1.status
    );
    assert!(
        snap.sessions.is_empty(),
        "no session after hold lift without peer: {:?}",
        snap.sessions
    );
}
