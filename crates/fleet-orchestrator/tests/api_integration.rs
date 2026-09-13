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
    app_and_store().await.0
}

async fn app_and_store() -> (axum::Router, fleet_orchestrator::store::Store) {
    let url = require_database_url();
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = Config {
        database_url: url,
        admin_token: "test-admin".into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
        orchestrator_public_url: "http://192.0.2.10:8080".into(),
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
    let store = state.store.clone();
    (router(state), store)
}

/// Closed pool: AdminAuth never acquires, so these 401s do not need DATABASE_URL.
async fn app_closed_pool(admin_token: impl Into<String>) -> axum::Router {
    use fleet_orchestrator::api::AppState;
    use fleet_orchestrator::orch::TickRunner;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
        .expect("lazy pool that never talks to Postgres");
    pool.close().await;
    let store = Store::new(pool);
    let tick = Arc::new(TickRunner::new(
        store.clone(),
        std::env::temp_dir().join("fleet-orch-admin-auth-closed"),
        "http://127.0.0.1:0",
    ));
    router(AppState {
        store,
        tick,
        admin_token: admin_token.into(),
        auth_mode: AuthMode::Token,
    })
}

fn api_error_text(v: &Value) -> String {
    v.get("error")
        .and_then(|e| e.as_str())
        .or_else(|| v.get("raw").and_then(|e| e.as_str()))
        .unwrap_or("")
        .to_string()
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
async fn readyz_ok_status_ready_when_pool_acquires() {
    let app = app().await;
    let (st, v) = json_req(app, "GET", "/readyz", None, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ready");
    assert!(
        v.get("error").is_none(),
        "readyz 200 must not include error: {v}"
    );
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
    let pid = proj["id"].as_str().expect("project id").to_string();
    let pid_uuid = Uuid::parse_str(&pid).expect("project uuid");

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
    assert_eq!(body["run_id"], run_id.to_string(), "{body}");
    assert_eq!(body["plan_id"], plan_id.to_string(), "{body}");
    assert_eq!(body["status"], "accepted", "{body}");

    let store = store().await;
    seed_workspace(&store, pid_uuid).await;
    pair_online_host(&store, pid_uuid).await;

    let (st, t1) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t1}");
    assert!(
        Uuid::parse_str(t1["tick_id"].as_str().unwrap_or("")).is_ok(),
        "admin/tick must return tick_id: {t1}"
    );
    assert_eq!(t1["plans_materialized"], 1, "{t1}");
    assert_eq!(t1["tasks_materialized"], 2, "{t1}");
    assert!(
        t1["spawned"].as_u64().unwrap_or(0) >= 1,
        "first tick must spawn t1: {t1}"
    );

    report_success(&store, pid_uuid, "t1").await;
    let (st, t2) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t2}");
    assert!(
        t2["inbox_applied"].as_u64().unwrap_or(0) >= 1,
        "second tick must apply t1 report: {t2}"
    );
    assert!(
        t2["spawned"].as_u64().unwrap_or(0) >= 1,
        "second tick must spawn t2: {t2}"
    );

    report_success(&store, pid_uuid, "t2").await;
    let (st, t3) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t3}");
    assert!(
        t3["inbox_applied"].as_u64().unwrap_or(0) >= 1,
        "third tick must apply t2 report: {t3}"
    );
    assert!(
        t3["runs_terminated"].as_u64().unwrap_or(0) >= 1,
        "third tick must complete the run: {t3}"
    );

    let (st, tasks) = json_req(
        app.clone(),
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{tasks}");
    let list = tasks["tasks"].as_array().expect("tasks array");
    assert_eq!(list.len(), 2, "{tasks}");
    assert!(
        list.iter().all(|t| t["status"] == "done_success"),
        "{tasks}"
    );
    assert!(list.iter().any(|t| t["plan_task_id"] == "t1"), "{tasks}");
    assert!(list.iter().any(|t| t["plan_task_id"] == "t2"), "{tasks}");
    assert!(
        list.iter().all(|t| t["run_id"] == run_id.to_string()),
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
    assert_eq!(st, StatusCode::OK, "{runs}");
    let run_list = runs["runs"].as_array().expect("runs array");
    assert_eq!(run_list.len(), 1, "{runs}");
    assert_eq!(run_list[0]["id"], run_id.to_string(), "{runs}");
    assert_eq!(run_list[0]["status"], "completed", "{runs}");
    assert_eq!(run_list[0]["source"], "fixture", "{runs}");
}

#[tokio::test]
async fn admin_tick_unknown_project_404() {
    let app = app().await;
    let missing = Uuid::new_v4();
    let (st, v) = json_req(
        app,
        "POST",
        &format!("/v1/projects/{missing}/admin/tick"),
        Some("test-admin"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(api_error_text(&v), "project not found", "{v}");
}

#[tokio::test]
async fn fixture_run_unknown_project_404() {
    let app = app().await;
    let missing = Uuid::new_v4();
    let plan = json!({
        "plan_id": Uuid::new_v4(),
        "project_id": missing,
        "run_id": Uuid::new_v4(),
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [{
            "plan_task_id": "t1",
            "title": "Missing project",
            "depends_on": [],
            "worker_profile": "coding",
            "priority": 100
        }],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    let (st, v) = json_req(
        app,
        "POST",
        &format!("/v1/projects/{missing}/runs/fixture"),
        Some("test-admin"),
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(api_error_text(&v), "project not found", "{v}");
}

#[tokio::test]
async fn admin_tick_list_tasks_list_runs_require_admin_bearer() {
    let app = app().await;
    let pid = Uuid::new_v4();

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/admin/tick"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "POST admin/tick {v}");
    assert!(
        api_error_text(&v).contains("invalid admin token"),
        "POST admin/tick {v}"
    );

    let (st, v) = json_req(
        app.clone(),
        "GET",
        &format!("/v1/projects/{pid}/tasks"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "GET tasks {v}");
    assert!(
        api_error_text(&v).contains("invalid admin token"),
        "GET tasks {v}"
    );

    let (st, v) = json_req(app, "GET", &format!("/v1/projects/{pid}/runs"), None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "GET runs {v}");
    assert!(
        api_error_text(&v).contains("invalid admin token"),
        "GET runs {v}"
    );
}

/// POST /v1/projects with AUTH_MODE=token and ADMIN_TOKEN set is 401 when
/// Authorization is missing. AdminAuth rejects before create_project runs.
#[tokio::test]
async fn admin_auth_required() {
    let app = app_closed_pool("test-admin").await;
    let (st, v) = json_req(
        app,
        "POST",
        "/v1/projects",
        None,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "POST /v1/projects missing Bearer: {v}"
    );
    assert_eq!(
        api_error_text(&v),
        "invalid admin token",
        "POST /v1/projects missing Bearer: {v}"
    );
}

/// POST /v1/projects with a present Bearer that is not ADMIN_TOKEN is 401.
/// create_project never runs (closed pool would 500 if AdminAuth were skipped).
#[tokio::test]
async fn create_project_401_wrong_admin_bearer() {
    let app = app_closed_pool("test-admin").await;
    let (st, v) = json_req(
        app,
        "POST",
        "/v1/projects",
        Some("not-the-token"),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "POST /v1/projects Bearer not-the-token: {v}"
    );
    assert_eq!(
        api_error_text(&v),
        "invalid admin token",
        "POST /v1/projects Bearer not-the-token: {v}"
    );
}

/// Empty or whitespace ADMIN_TOKEN with AUTH_MODE=token disables /v1 automation:
/// any Bearer on POST /v1/projects is 401.
#[tokio::test]
async fn create_project_401_empty_admin_token_disables_automation() {
    for admin_token in ["", "   "] {
        let app = app_closed_pool(admin_token).await;
        let (st, v) = json_req(
            app,
            "POST",
            "/v1/projects",
            Some("any-bearer"),
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "POST /v1/projects AUTH_MODE=token ADMIN_TOKEN={admin_token:?} any Bearer: {v}"
        );
        assert_eq!(
            api_error_text(&v),
            "admin automation disabled (set ADMIN_TOKEN to enable /v1 bearer routes)",
            "POST /v1/projects AUTH_MODE=token ADMIN_TOKEN={admin_token:?} any Bearer: {v}"
        );
    }
}

/// POST tick / fixture / cancel / resume and PATCH config with no Authorization
/// are 401 so dropping `_admin: AdminAuth` on those handlers fails.
#[tokio::test]
async fn v1_admin_mutating_routes_401_without_bearer() {
    let app = app_closed_pool("test-admin").await;
    let pid = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let cases = [
        ("POST", format!("/v1/projects/{pid}/admin/tick"), None),
        (
            "POST",
            format!("/v1/projects/{pid}/runs/fixture"),
            Some(json!({
                "plan": {
                    "plan_id": Uuid::new_v4(),
                    "project_id": pid,
                    "run_id": Uuid::new_v4(),
                    "version": 1,
                    "status": "accepted",
                    "source": "fixture",
                    "tasks": [],
                    "policy": { "on_task_fail": "fail_run" }
                }
            })),
        ),
        (
            "POST",
            format!("/v1/projects/{pid}/runs/{run_id}/cancel"),
            None,
        ),
        (
            "POST",
            format!("/v1/projects/{pid}/tasks/{task_id}/resume"),
            Some(json!({ "message": "resume" })),
        ),
        (
            "PATCH",
            format!("/v1/projects/{pid}/config"),
            Some(json!({ "repo_url": "https://example.test/r.git" })),
        ),
    ];
    for (method, uri, body) in cases {
        let (st, v) = json_req(app.clone(), method, &uri, None, body).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} missing Bearer: {v}"
        );
        assert_eq!(
            api_error_text(&v),
            "invalid admin token",
            "{method} {uri} missing Bearer: {v}"
        );
    }
}

/// Project + spawning session so privileged-bearer POSTs can be checked for inbox writes.
async fn seed_project_with_worker_session(
    app: axum::Router,
    store: &Store,
    name_prefix: &str,
) -> (Uuid, Uuid) {
    use fleet_orchestrator::store::NewWorkerSession;

    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some("test-admin"),
        Some(json!({"name": format!("{name_prefix}-{}", Uuid::new_v4()), "max_agents": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj}");
    let pid = Uuid::parse_str(proj["id"].as_str().expect("project id")).expect("uuid");

    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let plan = json!({
        "plan_id": plan_id,
        "project_id": pid,
        "run_id": run_id,
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [{
            "plan_task_id": "t1",
            "title": "Privileged bearer is not worker auth",
            "depends_on": [],
            "worker_profile": "coding",
            "priority": 100
        }],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    let (st, body) = json_req(
        app,
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        Some("test-admin"),
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    store
        .materialize_plan(pid, plan_id)
        .await
        .expect("materialize");
    let snap = store.load_board_snapshot(pid).await.expect("snapshot");
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .expect("task t1");

    let session_id = Uuid::new_v4();
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id: pid,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: format!("fleet-worker-session-token-{}", Uuid::new_v4()),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/privileged-bearer-v1-worker".into()),
        })
        .await
        .expect("insert session");
    (pid, session_id)
}

/// Design §8.2: /v1 worker heartbeat/status is Bearer <that session's
/// worker_token> only. ADMIN_TOKEN must not authenticate the job worker.
#[tokio::test]
async fn machine_token_v1_worker_heartbeat_status_rejected() {
    let (app, store) = app_and_store().await;
    let (pid, session_id) =
        seed_project_with_worker_session(app.clone(), &store, "admin-v1").await;
    let reports_before = store
        .list_unapplied_reports(pid)
        .await
        .expect("snapshot reports");

    let hb = format!("/v1/projects/{pid}/workers/{session_id}/heartbeat");
    let st_path = format!("/v1/projects/{pid}/workers/{session_id}/status");

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &hb,
        Some("test-admin"),
        Some(json!({ "seq": 1, "progress_pct": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "heartbeat ADMIN_TOKEN {v}");
    assert_eq!(
        api_error_text(&v),
        "invalid worker session/token",
        "heartbeat ADMIN_TOKEN {v}"
    );

    let (st, v) = json_req(
        app,
        "POST",
        &st_path,
        Some("test-admin"),
        Some(json!({ "seq": 2, "status": "success" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "status ADMIN_TOKEN {v}");
    assert_eq!(
        api_error_text(&v),
        "invalid worker session/token",
        "status ADMIN_TOKEN {v}"
    );

    let reports = store
        .list_unapplied_reports(pid)
        .await
        .expect("reports after admin");
    assert_eq!(
        reports.len(),
        reports_before.len(),
        "ADMIN_TOKEN must not append a report: {reports:?}"
    );
}

/// consume_pair_token machine_token is not a session worker_token.
#[tokio::test]
async fn minted_flm_cannot_post_v1_worker_heartbeat_status() {
    let (app, store) = app_and_store().await;
    fleet_db::migrate(store.pool())
        .await
        .expect("fleet-db migrate");
    let (pid, session_id) = seed_project_with_worker_session(app.clone(), &store, "flm-v1").await;
    seed_workspace(&store, pid).await;
    let pair = generate_pair_token();
    worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), pid, "api-host".to_string(), None),
        &hash_token(&pair),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    let machine_token = worker_hosts::consume_pair_token(
        store.pool(),
        &pair,
        "api-host",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("pair")
    .machine_token;

    let reports_before = store
        .list_unapplied_reports(pid)
        .await
        .expect("snapshot reports");
    let hb = format!("/v1/projects/{pid}/workers/{session_id}/heartbeat");
    let st_path = format!("/v1/projects/{pid}/workers/{session_id}/status");

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &hb,
        Some(&machine_token),
        Some(json!({ "seq": 1, "progress_pct": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "heartbeat minted flm_ {v}");
    assert_eq!(
        api_error_text(&v),
        "invalid worker session/token",
        "heartbeat minted flm_ {v}"
    );

    let (st, v) = json_req(
        app,
        "POST",
        &st_path,
        Some(&machine_token),
        Some(json!({ "seq": 2, "status": "success" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "status minted flm_ {v}");
    assert_eq!(
        api_error_text(&v),
        "invalid worker session/token",
        "status minted flm_ {v}"
    );

    let reports = store
        .list_unapplied_reports(pid)
        .await
        .expect("reports after flm");
    assert_eq!(
        reports.len(),
        reports_before.len(),
        "minted flm_ must not append a report: {reports:?}"
    );
}

/// Unused pair token (worker_host_pair_tokens row) is not a session worker_token.
#[tokio::test]
async fn minted_flp_cannot_post_v1_worker_heartbeat() {
    let (app, store) = app_and_store().await;
    fleet_db::migrate(store.pool())
        .await
        .expect("fleet-db migrate");
    let (pid, session_id) = seed_project_with_worker_session(app.clone(), &store, "flp-v1").await;
    seed_workspace(&store, pid).await;
    let flp = generate_pair_token();
    worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), pid, "api-host".to_string(), None),
        &hash_token(&flp),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");

    let reports_before = store
        .list_unapplied_reports(pid)
        .await
        .expect("snapshot reports");
    let hb = format!("/v1/projects/{pid}/workers/{session_id}/heartbeat");

    let (st, v) = json_req(
        app,
        "POST",
        &hb,
        Some(&flp),
        Some(json!({ "seq": 1, "progress_pct": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "heartbeat unused flp_ {v}");
    assert_eq!(
        api_error_text(&v),
        "invalid worker session/token",
        "heartbeat unused flp_ {v}"
    );

    let reports = store
        .list_unapplied_reports(pid)
        .await
        .expect("reports after flp");
    assert_eq!(
        reports.len(),
        reports_before.len(),
        "unused flp_ must not append a report: {reports:?}"
    );
}

#[tokio::test]
async fn worker_io_team_chat_roundtrip_and_rejects_bad_token() {
    use fleet_orchestrator::store::NewWorkerSession;

    let (app, store) = app_and_store().await;
    let admin = "test-admin";
    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(admin),
        Some(json!({"name": format!("worker-io-{}", Uuid::new_v4()), "max_agents": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj}");
    let pid = proj["id"].as_str().expect("project id");
    let project_id = Uuid::parse_str(pid).expect("project uuid");
    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let plan = json!({
        "plan_id": plan_id,
        "project_id": pid,
        "run_id": run_id,
        "version": 1,
        "status": "accepted",
        "source": "fixture",
        "tasks": [{
            "plan_task_id": "t1",
            "title": "IO",
            "depends_on": [],
            "worker_profile": "coding",
            "priority": 100
        }],
        "policy": { "on_task_fail": "fail_run", "auto_sync_external": false }
    });
    let (st, body) = json_req(
        app.clone(),
        "POST",
        &format!("/v1/projects/{pid}/runs/fixture"),
        Some(admin),
        Some(json!({ "plan": plan })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    store
        .materialize_plan(project_id, plan_id)
        .await
        .expect("materialize");
    let snap = store.load_board_snapshot(project_id).await.expect("snap");
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .expect("t1");
    let session_id = Uuid::new_v4();
    let token = "fleet-worker-session-token-io-gate";
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "remote".into(),
            worker_token: token.into(),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/worker-io".into()),
        })
        .await
        .expect("insert session");

    let io = format!("/v1/projects/{pid}/workers/{session_id}/io");
    let (st, v) = json_req(
        app.clone(),
        "POST",
        &io,
        None,
        Some(json!({"op": "team_chat_list", "all": true})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "missing bearer: {v}");

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &io,
        Some("wrong-token"),
        Some(json!({"op": "team_chat_list", "all": true})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "bad token: {v}");

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &io,
        Some(token),
        Some(json!({"op": "team_chat_post", "to": "*", "body": "hello from tiny"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "post: {v}");
    assert_eq!(v["ok"], true, "{v}");

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &io,
        Some(token),
        Some(json!({"op": "team_chat_list", "all": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "list: {v}");
    let msgs = v["messages"].as_array().expect("messages");
    assert!(
        msgs.iter()
            .any(|m| m["body"].as_str() == Some("hello from tiny")),
        "{v}"
    );
}
