//! HTTP API integration tests (PR6). Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fleet_orchestrator::api::{build_app_state, router};
use fleet_orchestrator::config::{AuthMode, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;
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

async fn app() -> axum::Router {
    let url = require_database_url();
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = Config {
        database_url: url,
        admin_token: "test-admin".into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
        orchestrator_public_url: "http://127.0.0.1:0".into(),
        data_dir: dir.path().to_path_buf(),
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
        sandbox_backend: "stub".into(),
        stub_duration: Duration::from_millis(0),
        process_worker_argv: vec!["fleet-driver".into()],
        k8s: fleet_orchestrator::sandbox::K8sOpts::default(),
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    };
    // leak tempdir for process lifetime of test
    std::mem::forget(dir);
    let state = build_app_state(&cfg).await.expect("state");
    router(state)
}

async fn json_req(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let req = builder
        .body(match body {
            Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!({"raw": String::from_utf8_lossy(&bytes)}))
    };
    (status, v)
}

#[tokio::test]
async fn healthz_ok() {
    let app = app().await;
    let (st, v) = json_req(app, "GET", "/healthz", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn fixture_and_tick_happy_path() {
    let app = app().await;
    let token = "test-admin";

    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(token),
        Some(json!({"name": format!("api-{}", Uuid::new_v4()), "max_agents": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj}");
    let pid = proj["id"].as_str().unwrap().to_string();
    assert_eq!(proj["config"]["git_ref"], "main");
    assert!(proj["config"]["repo_url"].is_null());

    let (st, cfg) = json_req(
        app.clone(),
        "PATCH",
        &format!("/v1/projects/{pid}/config"),
        Some(token),
        Some(json!({"repo_url": "https://github.com/acme/app.git", "git_ref": "develop"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cfg}");
    assert_eq!(cfg["repo_url"], "https://github.com/acme/app.git");
    assert_eq!(cfg["git_ref"], "develop");

    let (st, cfg) = json_req(
        app.clone(),
        "GET",
        &format!("/v1/projects/{pid}/config"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cfg}");
    assert_eq!(cfg["repo_url"], "https://github.com/acme/app.git");

    let (st, bad) = json_req(
        app.clone(),
        "PATCH",
        &format!("/v1/projects/{pid}/config"),
        Some(token),
        Some(json!({"repo_url": "not a url"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");

    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let plan = json!({
        "plan_id": plan_id,
        "project_id": pid,
        "run_id": run_id,
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [
            {
                "plan_task_id": "t1",
                "title": "Root",
                "depends_on": [],
                "worker_profile": "coding",
                "priority": 100,
                "max_retries": 1
            },
            {
                "plan_task_id": "t2",
                "title": "Child",
                "depends_on": ["t1"],
                "worker_profile": "coding",
                "priority": 50
            }
        ],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });

    let (st, body) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        Some(token),
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");

    // A few ticks should complete the stub chain (duration 0)
    for _ in 0..4 {
        let (st, tick) = json_req(
            app.clone(),
            "POST",
            &format!("/v1/projects/{pid}/admin/tick"),
            Some(token),
            Some(json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{tick}");
    }

    let (st, tasks) = json_req(
        app.clone(),
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let list = tasks["tasks"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|t| t["status"] == "done_success"), "{tasks}");

    let (st, runs) = json_req(
        app,
        "GET",
        &format!("/v1/projects/{pid}/runs"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(runs["runs"][0]["status"], "completed");
}

#[tokio::test]
async fn admin_auth_required() {
    let app = app().await;
    let (st, _) = json_req(
        app,
        "POST",
        "/v1/projects",
        None,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}
