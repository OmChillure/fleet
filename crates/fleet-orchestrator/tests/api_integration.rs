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
    let store = state.store.clone();
    (router(state), store)
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

/// Design §8.2: worker heartbeat/status auth is Bearer <session worker_token>.
/// WorkerAuth rejects missing/empty Bearer. authenticate_worker_session /
/// verify_worker_token rejects a token that does not hash-match (wrong key
/// or truncated). Neither path may append a worker_report.
#[tokio::test]
async fn worker_protocol_rejects_missing_empty_wrong_key_and_truncated_token() {
    use fleet_orchestrator::store::NewWorkerSession;

    let (app, store) = app_and_store().await;
    let admin = "test-admin";

    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(admin),
        Some(json!({"name": format!("worker-auth-{}", Uuid::new_v4()), "max_agents": 1})),
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
            "title": "Auth gate",
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
    let snap = store
        .load_board_snapshot(project_id)
        .await
        .expect("snapshot");
    let task = snap
        .tasks
        .iter()
        .find(|t| t.plan_task_id == "t1")
        .expect("task t1");

    let session_id = Uuid::new_v4();
    let token = "fleet-worker-session-token-auth-gate";
    assert!(token.len() > 16, "token too short to truncate: {token}");
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: token.into(),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/worker-auth".into()),
        })
        .await
        .expect("insert session");

    let heartbeat = format!("/v1/projects/{pid}/workers/{session_id}/heartbeat");
    let status = format!("/v1/projects/{pid}/workers/{session_id}/status");
    let hb_body = |seq: i32| json!({ "seq": seq, "progress_pct": 1 });
    let st_body = |seq: i32| json!({ "seq": seq, "status": "running" });

    async fn reject(
        app: axum::Router,
        method_path: &str,
        token: Option<&str>,
        body: Value,
        expect_msg: &str,
    ) {
        let (st, v) = json_req(app, "POST", method_path, token, Some(body)).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "{method_path} token={token:?} {v}"
        );
        let msg = api_error_text(&v);
        assert!(
            msg.contains(expect_msg),
            "{method_path} token={token:?} want {expect_msg:?} got {msg:?} {v}"
        );
    }

    reject(
        app.clone(),
        &heartbeat,
        None,
        hb_body(1),
        "worker token required",
    )
    .await;
    reject(
        app.clone(),
        &status,
        None,
        st_body(2),
        "worker token required",
    )
    .await;
    reject(
        app.clone(),
        &heartbeat,
        Some(""),
        hb_body(3),
        "worker token required",
    )
    .await;
    reject(
        app.clone(),
        &status,
        Some(""),
        st_body(4),
        "worker token required",
    )
    .await;

    let wrong = "other-worker-session-token-auth-gate";
    reject(
        app.clone(),
        &heartbeat,
        Some(wrong),
        hb_body(5),
        "invalid worker session/token",
    )
    .await;
    reject(
        app.clone(),
        &status,
        Some(wrong),
        st_body(6),
        "invalid worker session/token",
    )
    .await;

    let trunc = &token[..16];
    reject(
        app.clone(),
        &heartbeat,
        Some(trunc),
        hb_body(7),
        "invalid worker session/token",
    )
    .await;
    reject(
        app.clone(),
        &status,
        Some(trunc),
        st_body(8),
        "invalid worker session/token",
    )
    .await;

    let reports = store
        .list_unapplied_reports(project_id)
        .await
        .expect("reports after rejects");
    assert!(
        reports.is_empty(),
        "rejected worker posts must not append a report: {reports:?}"
    );

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &heartbeat,
        Some(token),
        Some(hb_body(10)),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "valid token heartbeat: {v}");

    let (st, v) = json_req(app, "POST", &status, Some(token), Some(st_body(11))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "valid token status: {v}");

    let reports = store
        .list_unapplied_reports(project_id)
        .await
        .expect("reports after valid");
    assert_eq!(
        reports.len(),
        2,
        "valid token must append heartbeat+status: {reports:?}"
    );
    assert!(
        reports.iter().any(|r| r.seq == 10 && r.kind == "heartbeat"),
        "{reports:?}"
    );
    assert!(
        reports.iter().any(|r| r.seq == 11 && r.kind == "status"),
        "{reports:?}"
    );
    assert!(
        reports.iter().all(|r| r.session_id == session_id),
        "{reports:?}"
    );
}

/// Design §8.2: Bearer is that session's worker_token only.
/// Missing/empty Bearer and a wrong (or truncated) token → 401.
/// Valid token posted under a different project_id → 404 and must not
/// append a report on either project. Cookie JWT (and a cookie holding
/// the real worker token) must not authenticate /v1 worker routes.
#[tokio::test]
async fn worker_heartbeat_status_wrong_token_401_other_project_404() {
    use fleet_orchestrator::store::NewWorkerSession;

    let (app, store) = app_and_store().await;
    let admin = "test-admin";

    async fn seed_project_session(
        app: axum::Router,
        store: &fleet_orchestrator::store::Store,
        admin: &str,
        name_prefix: &str,
        token: &str,
    ) -> (Uuid, Uuid) {
        let (st, proj) = json_req(
            app.clone(),
            "POST",
            "/v1/projects",
            Some(admin),
            Some(json!({
                "name": format!("{name_prefix}-{}", Uuid::new_v4()),
                "max_agents": 1
            })),
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
                "title": "Isolation",
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
            Some(admin),
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
                worker_token: token.into(),
                worker_profile: "coding".into(),
                work_dir: Some("/tmp/worker-isolation".into()),
            })
            .await
            .expect("insert session");
        (pid, session_id)
    }

    let token = format!("fleet-worker-session-token-{}", Uuid::new_v4());
    assert!(token.len() > 16, "token too short to truncate: {token}");
    let (pid_a, session_id) =
        seed_project_session(app.clone(), &store, admin, "hb-iso-a", &token).await;

    let (st, proj_b) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(admin),
        Some(json!({
            "name": format!("hb-iso-b-{}", Uuid::new_v4()),
            "max_agents": 1
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj_b}");
    let pid_b = Uuid::parse_str(proj_b["id"].as_str().expect("project b id")).expect("uuid");
    assert_ne!(pid_a, pid_b);

    let session_before = store
        .get_session(session_id)
        .await
        .expect("get session")
        .expect("session exists");
    let last_hb_before = session_before.last_heartbeat_at;
    let reports_a_before = store
        .list_unapplied_reports(pid_a)
        .await
        .expect("reports A before");
    let reports_b_before = store
        .list_unapplied_reports(pid_b)
        .await
        .expect("reports B before");
    assert!(
        reports_a_before.is_empty() && reports_b_before.is_empty(),
        "fixture must start with no reports: a={reports_a_before:?} b={reports_b_before:?}"
    );

    let hb_a = format!("/v1/projects/{pid_a}/workers/{session_id}/heartbeat");
    let st_a = format!("/v1/projects/{pid_a}/workers/{session_id}/status");
    let hb_b = format!("/v1/projects/{pid_b}/workers/{session_id}/heartbeat");
    let st_b = format!("/v1/projects/{pid_b}/workers/{session_id}/status");
    let hb_body = |seq: i32| json!({ "seq": seq, "progress_pct": 1 });
    let st_body = |seq: i32| json!({ "seq": seq, "status": "running" });

    async fn post_with(
        app: axum::Router,
        uri: &str,
        bearer: Option<&str>,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method("POST").uri(uri);
        if let Some(t) = bearer {
            builder = builder.header("Authorization", format!("Bearer {t}"));
        }
        if let Some(c) = cookie {
            builder = builder.header("Cookie", c);
        }
        builder = builder.header("content-type", "application/json");
        let req = builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(json!({"raw": String::from_utf8_lossy(&bytes)}))
        };
        (status, v)
    }

    async fn expect_401(
        app: axum::Router,
        uri: &str,
        bearer: Option<&str>,
        cookie: Option<&str>,
        body: Value,
        expect_msg: &str,
    ) {
        let (st, v) = post_with(app, uri, bearer, cookie, body).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "{uri} bearer={bearer:?} cookie={cookie:?} {v}"
        );
        let msg = api_error_text(&v);
        assert!(
            msg.contains(expect_msg),
            "{uri} bearer={bearer:?} cookie={cookie:?} want {expect_msg:?} got {msg:?} {v}"
        );
    }

    // Missing / empty Bearer is not a session token.
    expect_401(
        app.clone(),
        &hb_a,
        None,
        None,
        hb_body(1),
        "worker token required",
    )
    .await;
    expect_401(
        app.clone(),
        &st_a,
        None,
        None,
        st_body(2),
        "worker token required",
    )
    .await;
    expect_401(
        app.clone(),
        &hb_a,
        Some(""),
        None,
        hb_body(3),
        "worker token required",
    )
    .await;
    expect_401(
        app.clone(),
        &st_a,
        Some(""),
        None,
        st_body(4),
        "worker token required",
    )
    .await;

    let wrong = format!("other-worker-session-token-{}", Uuid::new_v4());
    assert_ne!(wrong, token);
    expect_401(
        app.clone(),
        &hb_a,
        Some(&wrong),
        None,
        hb_body(5),
        "invalid worker session/token",
    )
    .await;
    expect_401(
        app.clone(),
        &st_a,
        Some(&wrong),
        None,
        st_body(6),
        "invalid worker session/token",
    )
    .await;

    let trunc = &token[..16];
    expect_401(
        app.clone(),
        &hb_a,
        Some(trunc),
        None,
        hb_body(7),
        "invalid worker session/token",
    )
    .await;
    expect_401(
        app.clone(),
        &st_a,
        Some(trunc),
        None,
        st_body(8),
        "invalid worker session/token",
    )
    .await;

    // Browser session cookie is not worker auth — even if it holds the real token.
    let cookie_jwt = "fleet_access=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyIiwidHlwIjoiYWNjZXNzIn0.cookie-jwt-must-not-auth-worker";
    let jwt_value = cookie_jwt
        .strip_prefix("fleet_access=")
        .expect("fleet_access cookie");
    for (uri, body) in [
        (hb_a.as_str(), hb_body(9)),
        (st_a.as_str(), st_body(10)),
        (hb_b.as_str(), hb_body(11)),
        (st_b.as_str(), st_body(12)),
    ] {
        expect_401(
            app.clone(),
            uri,
            None,
            Some(cookie_jwt),
            body,
            "worker token required",
        )
        .await;
    }
    let cookie_real = format!("fleet_access={token}");
    for (uri, body) in [
        (hb_a.as_str(), hb_body(13)),
        (st_a.as_str(), st_body(14)),
        (hb_b.as_str(), hb_body(15)),
        (st_b.as_str(), st_body(16)),
    ] {
        expect_401(
            app.clone(),
            uri,
            None,
            Some(&cookie_real),
            body,
            "worker token required",
        )
        .await;
    }
    expect_401(
        app.clone(),
        &hb_a,
        Some(jwt_value),
        Some(cookie_jwt),
        hb_body(17),
        "invalid worker session/token",
    )
    .await;
    expect_401(
        app.clone(),
        &st_a,
        Some(jwt_value),
        Some(cookie_jwt),
        st_body(18),
        "invalid worker session/token",
    )
    .await;

    async fn expect_404_no_report(app: axum::Router, uri: &str, token: &str, body: Value) {
        let (st, v) = json_req(app, "POST", uri, Some(token), Some(body)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri} {v}");
        let msg = api_error_text(&v);
        assert!(
            msg.contains("session not in project"),
            "{uri} want session-not-in-project got {msg:?} {v}"
        );
    }

    expect_404_no_report(app.clone(), &hb_b, &token, hb_body(19)).await;
    expect_404_no_report(app.clone(), &st_b, &token, st_body(20)).await;

    let reports_a = store
        .list_unapplied_reports(pid_a)
        .await
        .expect("reports A after rejects");
    let reports_b = store
        .list_unapplied_reports(pid_b)
        .await
        .expect("reports B after rejects");
    assert!(
        reports_a.is_empty(),
        "rejected/wrong-project posts must not append on home project: {reports_a:?}"
    );
    assert!(
        reports_b.is_empty(),
        "rejected/wrong-project posts must not append on the other project: {reports_b:?}"
    );
    let session_after_rejects = store
        .get_session(session_id)
        .await
        .expect("get session after rejects")
        .expect("session exists");
    assert_eq!(
        session_after_rejects.last_heartbeat_at, last_hb_before,
        "rejects must not bump last_heartbeat_at"
    );

    let (st, v) = json_req(app.clone(), "POST", &hb_a, Some(&token), Some(hb_body(30))).await;
    assert_eq!(
        st,
        StatusCode::ACCEPTED,
        "valid token heartbeat on home: {v}"
    );
    let (st, v) = json_req(app, "POST", &st_a, Some(&token), Some(st_body(31))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "valid token status on home: {v}");

    let reports_a = store
        .list_unapplied_reports(pid_a)
        .await
        .expect("reports A after valid");
    let reports_b = store
        .list_unapplied_reports(pid_b)
        .await
        .expect("reports B after valid");
    assert_eq!(
        reports_a.len(),
        2,
        "home project must append heartbeat+status only after the valid posts: {reports_a:?}"
    );
    assert!(
        reports_a
            .iter()
            .any(|r| r.seq == 30 && r.kind == "heartbeat" && r.session_id == session_id),
        "{reports_a:?}"
    );
    assert!(
        reports_a
            .iter()
            .any(|r| r.seq == 31 && r.kind == "status" && r.session_id == session_id),
        "{reports_a:?}"
    );
    assert!(
        reports_b.is_empty(),
        "valid home posts must not append on the other project: {reports_b:?}"
    );
}

/// Design §8.2: /v1 worker heartbeat/status is Bearer <that session's
/// worker_token> only. A host machine token (`flm_`), pair token (`flp_`),
/// or ADMIN_TOKEN must not authenticate the job worker — a stolen flm_
/// must not spoof liveness or mark a task complete.
#[tokio::test]
async fn machine_token_v1_worker_heartbeat_status_rejected() {
    use fleet_orchestrator::store::NewWorkerSession;

    let (app, store) = app_and_store().await;
    let admin = "test-admin";

    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(admin),
        Some(json!({"name": format!("flm-v1-{}", Uuid::new_v4()), "max_agents": 1})),
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
            "title": "Machine token is not worker auth",
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
    let worker_token = format!("fleet-worker-session-token-{}", Uuid::new_v4());
    store
        .insert_session_spawning(&NewWorkerSession {
            id: session_id,
            project_id: pid,
            run_id,
            board_task_id: task.id,
            attempt: 1,
            runtime_backend: "stub".into(),
            worker_token: worker_token.clone(),
            worker_profile: "coding".into(),
            work_dir: Some("/tmp/machine-token-v1-worker".into()),
        })
        .await
        .expect("insert session");

    let (st, proj_b) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(admin),
        Some(json!({"name": format!("flm-v1-other-{}", Uuid::new_v4()), "max_agents": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj_b}");
    let pid_b = Uuid::parse_str(proj_b["id"].as_str().expect("project b id")).expect("uuid");
    assert_ne!(pid, pid_b);

    let flm = format!("flm_{}", Uuid::new_v4().simple());
    let flp = format!("flp_{}", Uuid::new_v4().simple());
    assert!(flm.starts_with("flm_"), "{flm}");
    assert!(flp.starts_with("flp_"), "{flp}");
    assert_ne!(flm, worker_token);
    assert_ne!(flp, worker_token);
    assert_ne!(admin, worker_token);

    let reports_before = store
        .list_unapplied_reports(pid)
        .await
        .expect("snapshot reports");
    let report_len_before = reports_before.len();
    let session_before = store
        .get_session(session_id)
        .await
        .expect("get session")
        .expect("session exists");
    let last_hb_before = session_before.last_heartbeat_at;

    let hb = format!("/v1/projects/{pid}/workers/{session_id}/heartbeat");
    let st_path = format!("/v1/projects/{pid}/workers/{session_id}/status");
    let hb_b = format!("/v1/projects/{pid_b}/workers/{session_id}/heartbeat");
    let st_b = format!("/v1/projects/{pid_b}/workers/{session_id}/status");
    let hb_body = |seq: i32| json!({ "seq": seq, "progress_pct": 1 });
    let st_body = |seq: i32, status: &str| json!({ "seq": seq, "status": status });

    async fn post_with(
        app: axum::Router,
        uri: &str,
        bearer: Option<&str>,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method("POST").uri(uri);
        if let Some(t) = bearer {
            builder = builder.header("Authorization", format!("Bearer {t}"));
        }
        if let Some(c) = cookie {
            builder = builder.header("Cookie", c);
        }
        builder = builder.header("content-type", "application/json");
        let req = builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(json!({"raw": String::from_utf8_lossy(&bytes)}))
        };
        (status, v)
    }

    async fn expect_401(
        app: axum::Router,
        uri: &str,
        bearer: Option<&str>,
        cookie: Option<&str>,
        body: Value,
        expect_msg: &str,
    ) {
        let (st, v) = post_with(app, uri, bearer, cookie, body).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "{uri} bearer={bearer:?} cookie={cookie:?} {v}"
        );
        let msg = api_error_text(&v);
        assert!(
            msg.contains(expect_msg),
            "{uri} bearer={bearer:?} cookie={cookie:?} want {expect_msg:?} got {msg:?} {v}"
        );
    }

    for (bearer, seq_hb, seq_st) in [(flm.as_str(), 1, 2), (flp.as_str(), 3, 4), (admin, 5, 6)] {
        expect_401(
            app.clone(),
            &hb,
            Some(bearer),
            None,
            hb_body(seq_hb),
            "invalid worker session/token",
        )
        .await;
        expect_401(
            app.clone(),
            &st_path,
            Some(bearer),
            None,
            st_body(seq_st, "success"),
            "invalid worker session/token",
        )
        .await;
    }

    let reports_after_stolen = store
        .list_unapplied_reports(pid)
        .await
        .expect("reports after flm/flp/admin");
    assert_eq!(
        reports_after_stolen.len(),
        report_len_before,
        "stolen host/admin tokens must not append a report: before={reports_before:?} after={reports_after_stolen:?}"
    );
    assert!(
        reports_after_stolen.is_empty(),
        "stolen host/admin tokens must not append a report: {reports_after_stolen:?}"
    );
    let session_after_stolen = store
        .get_session(session_id)
        .await
        .expect("get session after stolen")
        .expect("session exists");
    assert_eq!(
        session_after_stolen.last_heartbeat_at, last_hb_before,
        "stolen host/admin tokens must not bump last_heartbeat_at"
    );

    expect_401(
        app.clone(),
        &hb,
        None,
        Some("fleet_access=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyIiwidHlwIjoiYWNjZXNzIn0.cookie-jwt-must-not-auth-worker"),
        hb_body(7),
        "worker token required",
    )
    .await;
    expect_401(
        app.clone(),
        &st_path,
        None,
        Some(&format!("fleet_access={worker_token}")),
        st_body(8, "success"),
        "worker token required",
    )
    .await;

    let wrong = format!("other-worker-session-token-{}", Uuid::new_v4());
    expect_401(
        app.clone(),
        &hb,
        Some(&wrong),
        None,
        hb_body(9),
        "invalid worker session/token",
    )
    .await;
    expect_401(
        app.clone(),
        &st_path,
        Some(&wrong),
        None,
        st_body(10, "success"),
        "invalid worker session/token",
    )
    .await;

    for (uri, body) in [
        (hb_b.as_str(), hb_body(11)),
        (st_b.as_str(), st_body(12, "success")),
    ] {
        let (st, v) = json_req(app.clone(), "POST", uri, Some(&worker_token), Some(body)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri} {v}");
        let msg = api_error_text(&v);
        assert!(
            msg.contains("session not in project"),
            "{uri} want session-not-in-project got {msg:?} {v}"
        );
    }

    let reports_home = store
        .list_unapplied_reports(pid)
        .await
        .expect("home reports after rejects");
    let reports_other = store
        .list_unapplied_reports(pid_b)
        .await
        .expect("other reports after rejects");
    assert_eq!(
        reports_home.len(),
        report_len_before,
        "rejects must not append on home: {reports_home:?}"
    );
    assert!(
        reports_other.is_empty(),
        "valid token under another project_id must not append: {reports_other:?}"
    );
    let session_after_rejects = store
        .get_session(session_id)
        .await
        .expect("get session after rejects")
        .expect("session exists");
    assert_eq!(
        session_after_rejects.last_heartbeat_at, last_hb_before,
        "rejects must not bump last_heartbeat_at"
    );

    let (st, v) = json_req(
        app.clone(),
        "POST",
        &hb,
        Some(&worker_token),
        Some(hb_body(20)),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::ACCEPTED,
        "real worker_token must still heartbeat: {v}"
    );

    let reports_valid = store
        .list_unapplied_reports(pid)
        .await
        .expect("reports after valid heartbeat");
    assert_eq!(
        reports_valid.len(),
        report_len_before + 1,
        "real worker_token must append only after the valid heartbeat: {reports_valid:?}"
    );
    assert!(
        reports_valid
            .iter()
            .any(|r| r.seq == 20 && r.kind == "heartbeat" && r.session_id == session_id),
        "{reports_valid:?}"
    );
    let reports_other = store
        .list_unapplied_reports(pid_b)
        .await
        .expect("other reports after valid");
    assert!(
        reports_other.is_empty(),
        "valid home heartbeat must not append on the other project: {reports_other:?}"
    );
}
