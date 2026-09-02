//! HTTP API integration tests (PR6). Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_orchestrator::api::{build_app_state, router};
use fleet_orchestrator::config::{AuthMode, Config};
use fleet_orchestrator::store::{Store, connect, migrate};
use http_body_util::BodyExt;
use serde_json::{Value, json};
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

async fn store() -> Store {
    let url = require_database_url();
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("migrate");
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    Store::new(pool)
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("api{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("api-int")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_online_host(store: &Store, workspace_id: Uuid) {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "api-host".to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        "api-host",
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
        Some("api-host"),
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

async fn report_success(store: &Store, project_id: Uuid, plan_task_id: &str) {
    let snap = store.load_board_snapshot(project_id).await.unwrap();
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == plan_task_id)
        .unwrap_or_else(|| panic!("missing {plan_task_id}"));
    let session = if let Some(sid) = task.claimed_session_id {
        snap.sessions.iter().find(|s| s.id == sid)
    } else {
        snap.sessions
            .iter()
            .filter(|s| s.board_task_id == task.id)
            .max_by_key(|s| s.attempt)
    }
    .unwrap_or_else(|| panic!("no session for {plan_task_id}"));
    store
        .append_worker_report(
            project_id,
            session.id,
            task.id,
            session.attempt,
            "status",
            json!({
                "status": "success",
                "result_md": "Task completed successfully with artifacts.",
                "files": {"work/out.txt": "ok"}
            }),
        )
        .await
        .unwrap();
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

    let pid_uuid = Uuid::parse_str(&pid).expect("project uuid");
    let store = store().await;
    seed_workspace(&store, pid_uuid).await;
    pair_online_host(&store, pid_uuid).await;

    let tick = |app: axum::Router| {
        let pid = pid.clone();
        async move {
            json_req(
                app,
                "POST",
                &format!("/v1/projects/{pid}/admin/tick"),
                Some(token),
                Some(json!({})),
            )
            .await
        }
    };

    let (st, t1) = tick(app.clone()).await;
    assert_eq!(st, StatusCode::OK, "{t1}");
    report_success(&store, pid_uuid, "t1").await;
    let (st, t2) = tick(app.clone()).await;
    assert_eq!(st, StatusCode::OK, "{t2}");
    report_success(&store, pid_uuid, "t2").await;
    let (st, t3) = tick(app.clone()).await;
    assert_eq!(st, StatusCode::OK, "{t3}");

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
    assert!(
        list.iter().all(|t| t["status"] == "done_success"),
        "{tasks}"
    );

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
