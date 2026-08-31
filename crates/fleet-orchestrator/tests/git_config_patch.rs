//! Docs §6.9 / §8.2: admin PATCH `/v1/projects/{id}/config` shares cookie
//! `PATCH …/fleet/config` semantics — empty URL clears; git_ref without URL
//! is 400; garbage URL is 400. Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fleet_orchestrator::api::{build_app_state, router};
use fleet_orchestrator::config::{AuthMode, Config};
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
        sandbox_backend: "stub".into(),
        stub_duration: Duration::from_millis(0),
        process_worker_argv: vec!["fleet-driver".into()],
        k8s: fleet_orchestrator::sandbox::K8sOpts::default(),
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    };
    std::mem::forget(dir);
    let state = build_app_state(&cfg).await.expect("state");
    let store = state.store.clone();
    (router(state), store)
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

/// Docs §6.9 / §8.2: admin PATCH matches cookie PATCH for clear / 400s.
#[tokio::test]
async fn admin_patch_config_empty_url_clears_git_ref_without_url_and_garbage_url_are_400() {
    let (app, store) = app_and_store().await;
    let token = "test-admin";
    let url = "https://github.com/acme/app.git";

    let (st, proj) = json_req(
        app.clone(),
        "POST",
        "/v1/projects",
        Some(token),
        Some(json!({"name": format!("git-cfg-{}", Uuid::new_v4()), "max_agents": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{proj}");
    let pid = proj["id"].as_str().expect("project id").to_string();
    let project_id = Uuid::parse_str(&pid).expect("project uuid");
    let cfg_uri = format!("/v1/projects/{pid}/config");
    assert!(
        proj["config"]["repo_url"].is_null(),
        "new project has no remote: {proj}"
    );

    let ref_only_new = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(
        ref_only_new.0,
        StatusCode::BAD_REQUEST,
        "git_ref without URL on a new project: {}",
        ref_only_new.1
    );
    let err = ref_only_new.1["error"].as_str().unwrap_or("");
    assert!(
        err.contains("repo_url"),
        "ref-without-URL must mention repo_url: {}",
        ref_only_new.1
    );
    let still_none = store
        .get_project_config(project_id)
        .await
        .expect("config after ref-only 400");
    assert!(
        still_none.git_remote().is_none(),
        "ref-only 400 must not bind a remote: {:?}",
        still_none.repo_url
    );

    let (st, set) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "repo_url": url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "set remote: {set}");
    assert_eq!(set["repo_url"], url);
    assert_eq!(set["git_ref"], "develop");
    let stored = store
        .get_project_config(project_id)
        .await
        .expect("config after set");
    let remote = stored
        .git_remote()
        .expect("set remote must persist as git_remote()");
    assert_eq!(remote.url, url);
    assert_eq!(remote.git_ref, "develop");

    let (st, ref_ok) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "git_ref with existing URL: {ref_ok}");
    assert_eq!(ref_ok["repo_url"], url);
    assert_eq!(ref_ok["git_ref"], "feature");

    for bad in [
        "not a url",
        "-e evil",
        "ftp://example.com/r.git",
        "https://example.com/has space.git",
        "javascript:alert(1)",
    ] {
        let (st, out) = json_req(
            app.clone(),
            "PATCH",
            &cfg_uri,
            Some(token),
            Some(json!({ "repo_url": bad })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "invalid repo_url={bad:?} must 400: {out}"
        );
        let err = out["error"].as_str().unwrap_or("");
        assert!(
            err.contains("repo_url"),
            "invalid repo_url={bad:?} error should mention repo_url: {out}"
        );
        let still = store
            .get_project_config(project_id)
            .await
            .expect("config after invalid PATCH");
        let still_remote = still
            .git_remote()
            .expect("invalid URL must leave the remote bound");
        assert_eq!(still_remote.url, url);
        assert_eq!(still_remote.git_ref, "feature");
    }

    let (st, cleared) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "repo_url": "" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "empty URL: {cleared}");
    assert!(
        cleared["repo_url"].is_null()
            || cleared["repo_url"]
                .as_str()
                .map(str::trim)
                .unwrap_or("")
                .is_empty(),
        "empty repo_url must clear: {cleared}"
    );
    let after_clear = store
        .get_project_config(project_id)
        .await
        .expect("config after clear");
    assert!(
        after_clear.git_remote().is_none(),
        "empty URL must drop git_remote() so new tasks do not clone: {:?}",
        after_clear.repo_url
    );

    let (st, got) = json_req(app.clone(), "GET", &cfg_uri, Some(token), None).await;
    assert_eq!(st, StatusCode::OK, "{got}");
    assert!(
        got["repo_url"].is_null()
            || got["repo_url"]
                .as_str()
                .map(str::trim)
                .unwrap_or("")
                .is_empty(),
        "GET after clear must show no remote: {got}"
    );

    let (st, ref_only) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "git_ref": "other" })),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "git_ref without URL: {ref_only}"
    );
    let err = ref_only["error"].as_str().unwrap_or("");
    assert!(
        err.contains("repo_url"),
        "ref-without-URL error should mention repo_url: {ref_only}"
    );
    let still_clear = store
        .get_project_config(project_id)
        .await
        .expect("config after ref-only 400");
    assert!(
        still_clear.git_remote().is_none(),
        "ref-only 400 must not resurrect the remote: {:?}",
        still_clear.repo_url
    );

    let (st, set_again) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "repo_url": url, "git_ref": "develop" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{set_again}");
    let (st, clear_with_ref) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(token),
        Some(json!({ "repo_url": "", "git_ref": "other" })),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "empty URL + git_ref must clear, not 400: {clear_with_ref}"
    );
    let after_empty_with_ref = store
        .get_project_config(project_id)
        .await
        .expect("config after empty+ref");
    assert!(
        after_empty_with_ref.git_remote().is_none(),
        "empty URL + git_ref must drop git_remote(): {:?}",
        after_empty_with_ref.repo_url
    );
}
