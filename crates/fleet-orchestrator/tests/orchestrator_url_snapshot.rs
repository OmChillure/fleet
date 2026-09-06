//! QA-R17: spawn `FLEET_ORCHESTRATOR_URL` is the snapshotted Config / TickRunner
//! listener, not a live re-read of `API_PUBLIC_BASE` (Kind :8080).
//!
//! Required by `just test-integration` / merge-to-main. Requires DATABASE_URL.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_orchestrator::api::{build_app_state, build_app_state_with_pool_io};
use fleet_orchestrator::config::{AuthMode, Config};
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{connect, migrate, Store};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;
use uuid::Uuid;

const KIND_API_PUBLIC_BASE: &str = "http://127.0.0.1:8080";

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

async fn connect_store() -> Store {
    let url = require_database_url();
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("orch migrate");
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    Store::new(pool)
}

fn sample_config(database_url: String, data_dir: std::path::PathBuf, orch_url: &str) -> Config {
    Config {
        database_url,
        admin_token: "test-admin".into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
        orchestrator_public_url: orch_url.into(),
        data_dir,
        tick_interval: Duration::from_secs(5),
        max_agents: 3,
        heartbeat_stale: Duration::from_secs(60),
        spawn_timeout: Duration::from_secs(30),
        kill_grace: Duration::from_secs(0),
        default_task_timeout: Duration::from_secs(3600),
        default_max_retries: 2,
        retry_backoff: Duration::from_secs(10),
        on_task_fail: "fail_run".into(),
        auto_sync_external: false,
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    }
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
    let slug = format!("ou{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("orch-url")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(store: &Store, workspace_id: Uuid) {
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

async fn insert_plan(store: &Store, project_id: Uuid) {
    store
        .insert_accepted_plan(project_id, one_task_plan(project_id))
        .await
        .unwrap();
}

async fn tick_spawn_claim(store: &Store, tick: &TickRunner, project_id: Uuid) -> serde_json::Value {
    let r = tick.tick_project(project_id).await.unwrap();
    assert!(
        r.spawned >= 1,
        "expected a remote enqueue so we can inspect FLEET_ORCHESTRATOR_URL: {r:?}"
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

fn claim_orchestrator_url(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("env")
        .and_then(|e| e.get("FLEET_ORCHESTRATOR_URL"))
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("orchestrator_url").and_then(|v| v.as_str()))
}

async fn seed_ready_project(store: &Store) -> Uuid {
    let project = store
        .create_project(&format!("ou-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(store, project.id).await;
    pair_host(store, project.id).await;
    insert_plan(store, project.id).await;
    project.id
}

/// Config.orchestrator_public_url is cloned into TickRunner; later Kind
/// `API_PUBLIC_BASE` must not become worker `FLEET_ORCHESTRATOR_URL`.
#[tokio::test]
async fn build_app_state_worker_env_uses_config_url_after_api_public_base_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let snapshot = "http://192.0.2.10:18080";
    let cfg = sample_config(
        require_database_url(),
        dir.path().to_path_buf(),
        snapshot,
    );

    let _guard = env_lock().lock().await;
    let _restore = apply_env(&[
        ("API_PUBLIC_BASE", Some(KIND_API_PUBLIC_BASE)),
        ("ORCHESTRATOR_PUBLIC_URL", None),
    ]);

    let state = build_app_state(&cfg).await.expect("build_app_state");
    fleet_db::migrate(state.store.pool())
        .await
        .expect("fleet-db migrate");
    let project_id = seed_ready_project(&state.store).await;
    let payload = tick_spawn_claim(&state.store, &state.tick, project_id).await;
    assert_eq!(
        claim_orchestrator_url(&payload),
        Some(snapshot),
        "spawn must keep Config.orchestrator_public_url after API_PUBLIC_BASE=Kind :8080: {payload}"
    );
}

/// TickRunner ctor URL is what `schedule_and_spawn` stamps on the claim,
/// not a live `public_orchestrator_url` / `API_PUBLIC_BASE` re-read.
#[tokio::test]
async fn tick_runner_schedule_and_spawn_uses_ctor_url_not_live_env() {
    let store = connect_store().await;
    let project_id = seed_ready_project(&store).await;
    let snapshot = "http://listener.test:9";
    let tick = TickRunner::new(
        store.clone(),
        std::env::temp_dir().join(format!("fleet-orch-url-{}", Uuid::new_v4())),
        snapshot,
    );

    let _guard = env_lock().lock().await;
    let _restore = apply_env(&[
        ("API_PUBLIC_BASE", Some(KIND_API_PUBLIC_BASE)),
        ("ORCHESTRATOR_PUBLIC_URL", None),
    ]);

    let payload = tick_spawn_claim(&store, &tick, project_id).await;
    assert_eq!(
        claim_orchestrator_url(&payload),
        Some(snapshot),
        "schedule_and_spawn must keep TickRunner ctor URL after API_PUBLIC_BASE=Kind :8080: {payload}"
    );
}

/// Live fleet-server boot (`from_env_for_server` then `build_app_state_with_pool_io`)
/// keeps the snapshotted listener when `.env` later points at Kind :8080.
#[tokio::test]
async fn from_env_for_server_then_build_app_state_ignores_later_kind_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_str().expect("utf-8 DATA_DIR").to_owned();
    let snapshot = "http://orch.example.test:18080";
    let db_url = require_database_url();

    let _guard = env_lock().lock().await;
    let _restore = apply_env(&[
        ("API_PUBLIC_BASE", Some(snapshot)),
        ("ORCHESTRATOR_PUBLIC_URL", None),
        ("ORCHESTRATOR_DATABASE_URL", None),
        ("DATA_DIR", Some(data_dir.as_str())),
        ("ADMIN_TOKEN", Some("test-admin")),
        ("ORCH_AUTH_MODE", Some("token")),
    ]);
    let cfg = Config::from_env_for_server(&db_url, "127.0.0.1:18080")
        .expect("from_env_for_server");
    assert_eq!(
        cfg.orchestrator_public_url, snapshot,
        "from_env_for_server must snapshot API_PUBLIC_BASE"
    );

    // SAFETY: held under env_lock; EnvRestore still owns the prior value.
    unsafe {
        std::env::set_var("API_PUBLIC_BASE", KIND_API_PUBLIC_BASE);
    }

    let pool = connect(&cfg.database_url).await.expect("connect");
    migrate(&pool).await.expect("orch migrate");
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    let state = build_app_state_with_pool_io(&cfg, pool, None)
        .await
        .expect("build_app_state_with_pool_io");
    let project_id = seed_ready_project(&state.store).await;
    let payload = tick_spawn_claim(&state.store, &state.tick, project_id).await;
    assert_eq!(
        claim_orchestrator_url(&payload),
        Some(snapshot),
        "worker URL must stay orch.example.test after later Kind API_PUBLIC_BASE: {payload}"
    );
}
