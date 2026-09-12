//! Guest spawn-claim env: plan LLM stamps `FLEET_HARNESS` / `FLEET_MODEL`
//! after pass_through omits the plane key. Plane `FLEET_HARNESS` must not win.
//!
//! Required by `just test-integration` / merge-to-main. Requires DATABASE_URL.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{connect, migrate, Store};
use std::sync::OnceLock;
use tokio::sync::Mutex;
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
    TickRunner::new(
        store,
        std::env::temp_dir().join(format!("fleet-orch-guest-harness-{}", Uuid::new_v4())),
        "http://api.example.test:8080",
    )
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
            title: "T1".into(),
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
            max_agents_override: Some(8),
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("gh{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("guest-harness")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(store: &Store, workspace_id: Uuid) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "box".to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        "box",
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
        Some("box"),
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

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serializes env mutation (QA-R17/R20). Restored on drop / panic.
struct EnvRestore {
    previous: Vec<(String, Option<String>)>,
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        // SAFETY: held under env_lock; restore prior process env before unlock.
        unsafe {
            for (k, v) in &self.previous {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

fn apply_env(pairs: &[(&str, Option<&str>)]) -> EnvRestore {
    let previous: Vec<(String, Option<String>)> = pairs
        .iter()
        .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
        .collect();
    // SAFETY: held under env_lock; EnvRestore restores before unlock / panic resume.
    unsafe {
        for (k, v) in pairs {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
    EnvRestore { previous }
}

async fn with_plane_env<F, Fut, T>(
    harness: Option<&str>,
    model: Option<&str>,
    f: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _guard = env_lock().lock().await;
    let _restore = apply_env(&[
        ("FLEET_HARNESS", harness),
        ("FLEET_MODEL", model),
        ("FLEET_AGENT_BACKEND", None),
    ]);
    f().await
}

async fn insert_plan(
    store: &Store,
    project_id: Uuid,
    llm_provider: Option<&str>,
    llm_model: Option<&str>,
) {
    let mut plan = one_task_plan(project_id);
    plan.policy.llm_provider = llm_provider.map(str::to_string);
    plan.policy.llm_model = llm_model.map(str::to_string);
    store.insert_accepted_plan(project_id, plan).await.unwrap();
}

async fn tick_spawn_claim(store: &Store, project_id: Uuid) -> serde_json::Value {
    let r = runner(store.clone())
        .tick_project(project_id)
        .await
        .unwrap();
    assert!(
        r.spawned >= 1,
        "expected a remote enqueue so we can inspect the claim env: {r:?}"
    );
    sqlx::query_scalar(
        "SELECT payload FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_one(store.pool())
    .await
    .expect("spawn claim")
}

fn claim_env<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get("env")
        .and_then(|e| e.get(key))
        .and_then(|v| v.as_str())
}

async fn seed_project() -> (Store, Uuid) {
    let store = setup().await;
    let project = store
        .create_project(&format!("gh-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let _ = pair_host(&store, project.id).await;
    (store, project.id)
}

#[tokio::test]
async fn grok_plan_sets_guest_harness_even_when_plane_env_is_mock() {
    let (store, project_id) = seed_project().await;
    insert_plan(&store, project_id, Some("grok"), Some("grok-4.5")).await;

    let payload = with_plane_env(Some("mock"), None, || async {
        tick_spawn_claim(&store, project_id).await
    })
    .await;
    assert_eq!(
        claim_env(&payload, "FLEET_HARNESS"),
        Some("grok"),
        "plan LLM is grok; plane FLEET_HARNESS=mock must not win: {payload}"
    );
}

#[tokio::test]
async fn absent_plan_llm_does_not_inherit_plane_fleet_harness_grok() {
    let (store, project_id) = seed_project().await;
    insert_plan(&store, project_id, None, None).await;

    let payload = with_plane_env(Some("grok"), None, || async {
        tick_spawn_claim(&store, project_id).await
    })
    .await;
    assert_eq!(
        claim_env(&payload, "FLEET_HARNESS"),
        Some("mock"),
        "unset plan llm_provider must fill mock; plane FLEET_HARNESS=grok must not win: {payload}"
    );
}

#[tokio::test]
async fn grok_plan_sets_fleet_model_on_spawn_claim() {
    let (store, project_id) = seed_project().await;
    insert_plan(&store, project_id, Some("grok"), Some("grok-4.5")).await;

    let payload = with_plane_env(None, None, || async {
        tick_spawn_claim(&store, project_id).await
    })
    .await;
    assert_eq!(
        claim_env(&payload, "FLEET_MODEL"),
        Some("grok-4.5"),
        "plan llm_model must land on spawn_session payload.env.FLEET_MODEL: {payload}"
    );
}

#[tokio::test]
async fn ollama_plan_guest_harness_is_mock() {
    let (store, project_id) = seed_project().await;
    insert_plan(&store, project_id, Some("ollama"), None).await;

    let payload = with_plane_env(None, None, || async {
        tick_spawn_claim(&store, project_id).await
    })
    .await;
    assert_eq!(
        claim_env(&payload, "FLEET_HARNESS"),
        Some("mock"),
        "plan llm_provider=ollama is not grok; guest harness must be mock: {payload}"
    );
}
