//! In-process mock runner: lease a `spawn_session`, run the Ollama guest
//! against [`fleet_driver::mock_ollama`], post proof. No cloud provider.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::work_claims;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_driver::mock_ollama::{MockOllama, TEST_MODEL};
use fleet_driver::ollama_loop::run_ollama_on;
use fleet_driver::orch::OrchClient;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{connect, migrate, Store};
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

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("mr{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, min_isolation, created_at, updated_at)
        VALUES ($1, $2, $3, 'process', now(), now())
        ON CONFLICT (id) DO UPDATE SET min_isolation = 'process'
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("mock-runner")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(store: &Store, workspace_id: Uuid) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "mock-runner", None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        "mock-runner",
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
        Some("mock-runner"),
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

fn ollama_plan(project_id: Uuid) -> WorkPlanBody {
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: "Write a marker".into(),
            summary: String::new(),
            spec_md: "Write work/out.txt and result.md.".into(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(120),
            max_retries: Some(0),
        }],
        policy: PlanPolicy {
            max_agents_override: Some(2),
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            llm_provider: Some("ollama".into()),
            llm_model: Some(TEST_MODEL.into()),
            run_mode: "on_pc".into(),
            ..Default::default()
        },
    }
}

/// Lease the host's spawn claim, run the Ollama guest against the mock, post proof.
async fn mock_runner_drive(store: &Store, host_id: Uuid, ollama: &MockOllama) {
    let claim = work_claims::claim_next_spawn_for_host(
        store.pool(),
        Uuid::nil(),
        host_id,
        ChronoDuration::seconds(60),
    )
    .await
    .expect("lease")
    .expect("spawn_session waiting for mock runner");
    assert_eq!(claim.kind, "spawn_session");
    let harness = claim.payload["env"]["FLEET_HARNESS"].as_str();
    assert_eq!(
        harness,
        Some("ollama"),
        "mock runner only runs the local Ollama guest: {}",
        claim.payload
    );
    assert_eq!(
        claim.payload["env"]["FLEET_MODEL"].as_str(),
        Some(TEST_MODEL),
        "must not pin a cloud SKU: {}",
        claim.payload
    );

    let work = tempfile::tempdir().expect("work");
    let files = claim
        .payload
        .get("workspace_files")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut packed = std::collections::BTreeMap::new();
    for (k, v) in files {
        if let Some(s) = v.as_str() {
            packed.insert(k, s.to_string());
        }
    }
    agent_fleet_contracts::write_guest_files(work.path(), &packed).expect("materialize");
    if let Some(spec) = claim.payload["task_spec"].as_str().filter(|s| !s.is_empty()) {
        let _ = std::fs::write(work.path().join("prompt.md"), spec);
        let _ = std::fs::write(work.path().join("spec.md"), spec);
    }

    let session_id = claim.payload["session_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("session_id");
    let project_id = claim.payload["project_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("project_id");
    let board_task_id = claim.payload["board_task_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("board_task_id");
    let token = claim.payload["worker_token"].as_str().unwrap_or("tok");
    let client = OrchClient::new("http://127.0.0.1:1", project_id, session_id, token)
        .expect("orch client");
    let prompt = fleet_driver::prompt::load_prompt(work.path());
    run_ollama_on(
        work.path(),
        TEST_MODEL,
        &prompt,
        Duration::from_secs(8),
        &client,
        Some(&ollama.base_url()),
    )
    .await
    .expect("ollama guest on mock runner");

    let result_md = std::fs::read_to_string(work.path().join("result.md")).unwrap_or_default();
    let guest = agent_fleet_contracts::collect_guest_files(work.path());
    let files = agent_fleet_contracts::files_as_json_map(&guest);
    store
        .append_worker_report(
            project_id,
            session_id,
            board_task_id,
            1,
            "status",
            serde_json::json!({
                "status": "success",
                "result_md": result_md,
                "files": files,
            }),
        )
        .await
        .expect("proof");
    let tok = claim.lease_token.expect("lease_token");
    work_claims::complete_owned_with_result(
        store.pool(),
        claim.id,
        tok,
        host_id,
        claim.workspace_id,
        None,
    )
    .await
    .expect("complete");
}

#[tokio::test]
async fn mock_runner_runs_tiny_ollama_and_inbox_completes() {
    let store = setup().await;
    let project = store
        .create_project(&format!("mock-run-{}", Uuid::new_v4()), 2)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let host_id = pair_host(&store, project.id).await;
    store
        .insert_accepted_plan(project.id, ollama_plan(project.id))
        .await
        .unwrap();

    let tick = TickRunner::new(
        store.clone(),
        std::env::temp_dir().join("fleet-orch-mock-runner"),
        "http://192.0.2.10:8080",
    );
    let r1 = tick.tick_project(project.id).await.unwrap();
    assert!(r1.spawned >= 1, "tick must assign the mock runner: {r1:?}");

    let ollama = MockOllama::spawn().expect("mock ollama");
    mock_runner_drive(&store, host_id, &ollama).await;
    assert!(ollama.rounds() >= 1, "guest must have called the mock model");

    let r2 = tick.tick_project(project.id).await.unwrap();
    assert!(r2.inbox_applied >= 1, "proof must apply: {r2:?}");

    let snap = store.load_board_snapshot(project.id).await.unwrap();
    let t1 = snap.tasks.iter().find(|t| t.plan_task_id == "t1").unwrap();
    assert_eq!(
        t1.status, "done_success",
        "mock runner + tiny ollama must complete the task: {t1:?}"
    );
}
