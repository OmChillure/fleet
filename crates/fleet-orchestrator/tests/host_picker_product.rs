//! Product contracts for host assignment. These encode intended behavior
//! (lowest load/cap, oldest heartbeat, min_isolation, no loopback for
//! container guests, plan LLM → FLEET_HARNESS). They are allowed to fail
//! when the current picker/spawn path does not match.
//!
//! Requires DATABASE_URL.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_db::repos::workspaces;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{Store, connect, migrate, select_assignable_host};
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

fn runner(store: Store, orch_url: &str) -> TickRunner {
    TickRunner::new(
        store,
        std::env::temp_dir().join("fleet-orch-picker-product"),
        orch_url,
    )
}

fn one_task_plan(project_id: Uuid) -> WorkPlanBody {
    n_task_plan(project_id, 1)
}

fn n_task_plan(project_id: Uuid, n: usize) -> WorkPlanBody {
    let tasks = (1..=n)
        .map(|i| PlanTask {
            plan_task_id: format!("t{i}"),
            title: format!("T{i}"),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(0),
        })
        .collect();
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks,
        policy: PlanPolicy {
            max_agents_override: Some(8),
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("pp{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("picker-product")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(
    store: &Store,
    workspace_id: Uuid,
    name: &str,
    advertised_host: bool,
    advertised_container: bool,
) -> Uuid {
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
        advertised_host,
        advertised_container,
        None,
    )
    .await
    .expect("pair");
    worker_hosts::heartbeat(
        store.pool(),
        host.id,
        advertised_host,
        advertised_container,
        Some("linux-x86_64"),
        Some(name),
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
async fn picker_prefers_idle_host_over_busier_host_listed_first() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-load-{}", Uuid::new_v4()), 8)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let busy = pair_host(&store, project.id, "busy", true, false).await;
    let idle = pair_host(&store, project.id, "idle", true, false).await;
    // Newest-first SQL: make busy the first row.
    age_last_seen(&store, idle, 20).await;

    let plan = n_task_plan(project.id, 3);
    let plan_id = plan.plan_id;
    store.insert_accepted_plan(project.id, plan).await.unwrap();
    store
        .materialize_plan(project.id, plan_id)
        .await
        .expect("materialize");
    let tasks = store.list_tasks_slim(project.id, None).await.unwrap();
    for t in &tasks {
        let _ = store
            .update_task_status(t.id, "queued", "ready", None)
            .await;
        let assignment = store
            .begin_spawn_assignment(
                t.id,
                project.id,
                t.run_id,
                &t.worker_profile,
                "remote",
                None,
            )
            .await
            .unwrap()
            .expect("cas");
        store
            .record_remote_enqueued(assignment.session_id, &format!("host:{busy}"), None)
            .await
            .unwrap();
    }

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("idle host must still be assignable");
    assert_eq!(
        chosen.id(),
        idle,
        "3 in-flight on the newer host must lose to an idle older host (lowest load/cap)"
    );
}

#[tokio::test]
async fn picker_equal_load_prefers_oldest_heartbeat() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-age-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let older = pair_host(&store, project.id, "older", true, false).await;
    let newer = pair_host(&store, project.id, "newer", true, false).await;
    age_last_seen(&store, older, 15).await;

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("both online");
    assert_eq!(
        chosen.id(),
        older,
        "equal load: oldest heartbeat wins, not last_seen DESC"
    );
    let _ = newer;
}

#[tokio::test]
async fn container_min_isolation_skips_process_only_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-iso-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .expect("min_isolation=container");
    let _ = pair_host(&store, project.id, "proc-only", true, false).await;

    assert!(
        select_assignable_host(&store, project.id).await.is_none(),
        "a process-only host must not take a workspace that requires container isolation"
    );
}

#[tokio::test]
async fn vm_min_isolation_skips_container_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-vm-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("min_isolation=vm");
    let _ = pair_host(&store, project.id, "ctr", false, true).await;

    assert!(
        select_assignable_host(&store, project.id).await.is_none(),
        "min_isolation=vm must not fall back to a container host"
    );
}

#[tokio::test]
async fn container_need_picks_container_runtime_on_dual_host() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-dual-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .unwrap();
    let host = pair_host(&store, project.id, "both", true, true).await;

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("dual host meets container need");
    assert_eq!(chosen.id(), host);
    assert_eq!(
        chosen.runtime.as_str(),
        "container",
        "weakest class that meets container need is container, not process"
    );
}

#[tokio::test]
async fn tick_must_not_enqueue_container_work_to_a_loopback_plane() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-lb-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .unwrap();
    let _ = pair_host(&store, project.id, "ctr", false, true).await;
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session'",
    )
    .bind(project.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        r.spawned, 0,
        "loopback plane URL is unreachable from a container guest: {r:?}"
    );
    assert_eq!(
        claims, 0,
        "must not enqueue a claim the guest cannot heartbeat"
    );
}

async fn spawn_claim_count(store: &Store, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session'",
    )
    .bind(workspace_id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// Documented follow-up: fail-closed assign when the plane URL
/// (`WORKER_PUBLIC_BASE` / orchestrator URL) is loopback and the paired
/// host is not this machine. Off-box workers cannot heartbeat to
/// 127/8, localhost, or [::1]. Local fleet-dev (this-machine + loopback)
/// may still assign.
#[tokio::test]
async fn tick_must_not_assign_offbox_host_when_plane_url_is_loopback() {
    let store = setup().await;
    for url in [
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
        "http://127.1.2.3:8080",
    ] {
        let project = store
            .create_project(&format!("pp-lb-off-{}", Uuid::new_v4()), 4)
            .await
            .unwrap();
        seed_workspace(&store, project.id).await;
        let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
        store
            .insert_accepted_plan(project.id, one_task_plan(project.id))
            .await
            .unwrap();

        let r = runner(store.clone(), url)
            .tick_project(project.id)
            .await
            .unwrap();
        let claims = spawn_claim_count(&store, project.id).await;
        assert_eq!(
            r.spawned, 0,
            "must not assign off-box host {off} while plane URL {url} is loopback: {r:?}"
        );
        assert_eq!(
            claims, 0,
            "must not enqueue a spawn claim an off-box host cannot heartbeat \
             ({url}): spawned={:?} claims={claims}",
            r.spawned
        );
    }
}

/// Same-box loopback is local `fleet-dev`: a host whose hostname is this
/// machine may still take work when the plane URL is loopback.
#[tokio::test]
async fn tick_may_assign_this_machine_host_when_plane_url_is_loopback() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-lb-local-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let local_name = std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| panic!("this-machine loopback assign needs HOSTNAME or /etc/hostname"));
    let local = pair_host(&store, project.id, &local_name, true, false).await;
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims = spawn_claim_count(&store, project.id).await;
    assert!(
        r.spawned >= 1 || claims >= 1,
        "this-machine host {local} ({local_name}) must still be assignable \
         when the plane URL is loopback (local fleet-dev): {r:?} claims={claims}"
    );
}

#[tokio::test]
async fn grok_plan_sets_guest_harness_even_when_plane_env_is_mock() {
    let store = setup().await;
    let project = store
        .create_project(&format!("pp-harness-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let _ = pair_host(&store, project.id, "box", true, false).await;
    let mut plan = one_task_plan(project.id);
    plan.policy.llm_provider = Some("grok".into());
    plan.policy.llm_model = Some("grok-4.5".into());
    store.insert_accepted_plan(project.id, plan).await.unwrap();

    unsafe {
        std::env::set_var("FLEET_HARNESS", "mock");
    }
    let r = runner(store.clone(), "http://api.example.test:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    assert!(
        r.spawned >= 1,
        "expected a remote enqueue so we can inspect the claim env: {r:?}"
    );

    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project.id)
    .fetch_one(store.pool())
    .await
    .expect("spawn claim");
    let harness = payload
        .get("env")
        .and_then(|e| e.get("FLEET_HARNESS"))
        .and_then(|v| v.as_str());
    assert_eq!(
        harness,
        Some("grok"),
        "plan LLM is grok; plane FLEET_HARNESS=mock must not win: {payload}"
    );
}
