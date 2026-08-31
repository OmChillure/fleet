//! Control plane runner off → tick must not local-spawn.
//! Missing plane row keeps today's local fallback.
//! Requires DATABASE_URL.

use fleet_db::repos::plane_runners::{self, PlaneRunnerPatch};
use fleet_db::repos::workspaces;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::sandbox::StubRuntime;
use fleet_orchestrator::store::{Store, connect, migrate, select_assignable_runner};
use std::sync::Arc;
use std::time::Duration;
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
    let rt = Arc::new(StubRuntime::new(Duration::from_secs(60)));
    TickRunner::with_store_and_runtime(store, rt)
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
    let slug = format!("pr{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("plane-tick")
    .execute(store.pool())
    .await
    .expect("insert workspace");
    // Shared DATABASE_URL may have a cluster-labeled guest from another
    // workspace. VM min isolation is not offered by any host, so the picker
    // leaves these cases on the local plane path (catalog: no remote host).
    workspaces::set_min_isolation(store.pool(), project_id, "vm")
        .await
        .expect("min_isolation=vm");
}

#[tokio::test]
async fn tick_holds_ready_when_plane_not_taking_jobs() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-off-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    plane_runners::upsert(
        store.pool(),
        project.id,
        &PlaneRunnerPatch {
            allow_host: Some(false),
            allow_container: Some(false),
            ..PlaneRunnerPatch::default()
        },
    )
    .await
    .expect("plane off");
    assert!(
        !plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "stop taking jobs must set taking_native_jobs=false"
    );
    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available; plane off must still skip it"
    );

    let r = rnr.tick_project(project.id).await.unwrap();
    assert_eq!(r.spawned, 0, "plane off must not local-spawn: {r:?}");
    assert_eq!(r.spawn_failed, 0, "{r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "ready", "task must stay ready: {:?}", t1.status);
    assert!(
        snap.sessions.is_empty(),
        "no local session: {:?}",
        snap.sessions
    );
    assert_eq!(
        store.count_local_capacity(project.id).await.unwrap(),
        0,
        "no local SANDBOX_BACKEND capacity"
    );
}

#[tokio::test]
async fn tick_local_spawns_when_plane_taking_jobs() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-on-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    plane_runners::upsert(
        store.pool(),
        project.id,
        &PlaneRunnerPatch {
            allow_host: Some(true),
            allow_container: Some(false),
            ..PlaneRunnerPatch::default()
        },
    )
    .await
    .expect("plane on");
    assert!(
        plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "allow_host must set taking_native_jobs=true"
    );
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available"
    );
    let backend = rnr.runtime_backend();
    let r = rnr.tick_project(project.id).await.unwrap();
    assert!(
        r.spawned >= 1,
        "plane taking jobs + no remote must local-spawn: {r:?}"
    );
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let sess = snap.sessions.first().expect("local session");
    assert_ne!(sess.runtime_backend, "remote");
    assert_eq!(
        sess.runtime_backend, backend,
        "must spawn this process SANDBOX_BACKEND, got {}",
        sess.runtime_backend
    );
}

#[tokio::test]
async fn tick_local_spawns_when_plane_row_missing() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-def-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    assert!(
        plane_runners::get(store.pool(), project.id)
            .await
            .expect("get plane")
            .is_none(),
        "plane row must be missing so get_or_default applies"
    );
    assert!(
        plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "missing plane row defaults taking_native_jobs=true"
    );
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available"
    );
    let backend = rnr.runtime_backend();
    let r = rnr.tick_project(project.id).await.unwrap();
    assert!(
        r.spawned >= 1,
        "missing plane row keeps today's local spawn: {r:?}"
    );
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let sess = snap.sessions.first().expect("local session");
    assert_ne!(sess.runtime_backend, "remote");
    assert_eq!(
        sess.runtime_backend, backend,
        "must spawn this process SANDBOX_BACKEND, got {}",
        sess.runtime_backend
    );
}

/// Catalog: default/missing plane row + no assignable guest
/// → TickRunner local-spawns SANDBOX_BACKEND (not remote).
/// `schedule_and_spawn` via `tick_project`; picker None is the no-remote lock.
#[tokio::test]
async fn tick_local_spawns_when_plane_row_missing_no_remote() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-miss-nr-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    assert!(
        plane_runners::get(store.pool(), project.id)
            .await
            .expect("get plane")
            .is_none(),
        "plane row must be missing so get_or_default applies"
    );
    assert!(
        plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "missing plane row defaults taking_native_jobs=true"
    );
    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available"
    );
    let backend = rnr.runtime_backend();
    let r = rnr.tick_project(project.id).await.unwrap();
    assert!(
        r.spawned >= 1,
        "default/missing plane + no remote must local-spawn SANDBOX_BACKEND: {r:?}"
    );
    assert_eq!(r.spawn_failed, 0, "{r:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let sess = snap.sessions.first().expect("local session");
    assert_ne!(sess.runtime_backend, "remote");
    assert_eq!(
        sess.runtime_backend, backend,
        "must spawn this process SANDBOX_BACKEND, got {}",
        sess.runtime_backend
    );
}

/// Catalog: plane allow_host=false + no remote → hold ready, no local spawn.
/// Contrast so a TickRunner that always holds cannot pass the taking-jobs lock.
#[tokio::test]
async fn tick_holds_ready_when_plane_allow_host_false_no_remote() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-off-nr-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    plane_runners::upsert(
        store.pool(),
        project.id,
        &PlaneRunnerPatch {
            allow_host: Some(false),
            allow_container: Some(false),
            ..PlaneRunnerPatch::default()
        },
    )
    .await
    .expect("plane off");
    assert!(
        !plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "allow_host=false must set taking_native_jobs=false"
    );
    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available; plane off must still skip it"
    );

    let r = rnr.tick_project(project.id).await.unwrap();
    assert_eq!(
        r.spawned, 0,
        "plane off + no remote must not local-spawn: {r:?}"
    );
    assert_eq!(r.spawn_failed, 0, "{r:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(t1.status, "ready", "task must stay ready: {:?}", t1.status);
    assert!(
        snap.sessions.is_empty(),
        "no local session: {:?}",
        snap.sessions
    );
    assert_eq!(
        store.count_local_capacity(project.id).await.unwrap(),
        0,
        "no local SANDBOX_BACKEND capacity"
    );
}

/// Control: explicit allow_host=true + no remote still local-spawns.
/// A TickRunner that always holds would pass the plane-off case alone.
#[tokio::test]
async fn tick_local_spawns_when_plane_allow_host_true_no_remote() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-on-nr-{}", Uuid::new_v4()), 3)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    plane_runners::upsert(
        store.pool(),
        project.id,
        &PlaneRunnerPatch {
            allow_host: Some(true),
            allow_container: Some(false),
            ..PlaneRunnerPatch::default()
        },
    )
    .await
    .expect("plane on");
    assert!(
        plane_runners::taking_native_jobs(store.pool(), project.id)
            .await
            .expect("taking_native_jobs"),
        "allow_host must set taking_native_jobs=true"
    );
    let remote = select_assignable_runner(&store, project.id).await;
    assert!(remote.is_none(), "catalog: no remote host: {remote:?}");

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let rnr = runner(store.clone());
    assert_ne!(
        rnr.runtime_backend(),
        "remote",
        "in-process SANDBOX_BACKEND is available"
    );
    let backend = rnr.runtime_backend();
    let r = rnr.tick_project(project.id).await.unwrap();
    assert!(
        r.spawned >= 1,
        "explicit allow_host=true + no remote must local-spawn: {r:?}"
    );
    assert_eq!(r.spawn_failed, 0, "{r:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let sess = snap.sessions.first().expect("local session");
    assert_ne!(sess.runtime_backend, "remote");
    assert_eq!(
        sess.runtime_backend, backend,
        "must spawn this process SANDBOX_BACKEND, got {}",
        sess.runtime_backend
    );
}

fn two_task_plan(project_id: Uuid) -> WorkPlanBody {
    let mut body = one_task_plan(project_id);
    body.tasks.push(PlanTask {
        plan_task_id: "t2".into(),
        title: "Two".into(),
        summary: String::new(),
        spec_md: String::new(),
        depends_on: vec![],
        worker_profile: "coding".into(),
        acceptance: vec![],
        priority: 100,
        external_ref: None,
        timeout_s: Some(3600),
        max_retries: Some(0),
    });
    body
}

#[tokio::test]
async fn tick_honors_plane_max_concurrent() {
    let store = setup().await;
    let project = store
        .create_project(&format!("plane-cap-{}", Uuid::new_v4()), 8)
        .await
        .unwrap();
    seed_workspace_for_project(&store, project.id).await;
    plane_runners::upsert(
        store.pool(),
        project.id,
        &PlaneRunnerPatch {
            allow_host: Some(true),
            max_concurrent: Some(1),
            ..PlaneRunnerPatch::default()
        },
    )
    .await
    .expect("plane cap 1");

    store
        .insert_accepted_plan(project.id, two_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone())
        .tick_project(project.id)
        .await
        .unwrap();
    assert_eq!(r.spawned, 1, "plane max_concurrent=1: {r:?}");
    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let ready = snap.tasks.iter().filter(|t| t.status == "ready").count();
    assert_eq!(ready, 1, "second task stays ready: {:?}", snap.tasks);
    assert_eq!(store.count_local_capacity(project.id).await.unwrap(), 1);
}
