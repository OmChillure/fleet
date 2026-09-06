//! Docs §6.9 / §8.2: admin PATCH `/v1/projects/{id}/config` persist/400
//! second line — empty URL clears; git_ref without URL is 400; garbage URL
//! is 400 and leaves the stored remote. Requires DATABASE_URL.

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

const ADMIN: &str = "test-admin";
const REPO: &str = "https://github.com/acme/app.git";

async fn seed_project(store: &fleet_orchestrator::store::Store) -> Uuid {
    store
        .create_project(&format!("git-cfg-{}", Uuid::new_v4()), 3)
        .await
        .expect("project")
        .id
}

async fn bind_remote(
    store: &fleet_orchestrator::store::Store,
    project_id: Uuid,
    git_ref: &str,
) {
    store
        .set_project_git_remote(
            project_id,
            Some(&fleet_orchestrator::domain::ProjectGitRemote {
                url: REPO.into(),
                git_ref: git_ref.into(),
            }),
        )
        .await
        .expect("fixture bind remote");
}

fn config_uri(project_id: Uuid) -> String {
    format!("/v1/projects/{project_id}/config")
}

/// Empty repo_url 200 drops git_remote() so new tasks do not clone.
#[tokio::test]
async fn admin_patch_empty_url_clears() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "develop").await;
    let cfg_uri = config_uri(pid);

    let (st, cleared) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "repo_url": "" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "empty URL: {cleared}");
    assert!(
        cleared["repo_url"].is_null(),
        "empty repo_url must clear: {cleared}"
    );
    let after = store
        .get_project_config(pid)
        .await
        .expect("config after clear");
    assert!(
        after.git_remote().is_none(),
        "empty URL must drop git_remote() so new tasks do not clone: {:?}",
        after.repo_url
    );

    let (st, got) = json_req(app, "GET", &cfg_uri, Some(ADMIN), None).await;
    assert_eq!(st, StatusCode::OK, "{got}");
    assert!(
        got["repo_url"].is_null(),
        "GET after clear must show no remote: {got}"
    );
}

/// git_ref without a bound URL is 400 and does not bind a remote.
#[tokio::test]
async fn admin_patch_git_ref_without_url_is_400() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "git_ref": "feature" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "git_ref without URL: {out}");
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after ref-only 400");
    assert!(
        still.git_remote().is_none(),
        "ref-only 400 must not bind a remote: {:?}",
        still.repo_url
    );
}

/// One garbage repo_url is 400 and leaves the stored remote.
#[tokio::test]
async fn admin_patch_garbage_url_is_400_leaves_remote() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "feature").await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "repo_url": "not a url" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "garbage URL must 400: {out}");
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after invalid PATCH");
    let remote = still
        .git_remote()
        .expect("invalid URL must leave the remote bound");
    assert_eq!(remote.url, REPO);
    assert_eq!(remote.git_ref, "feature");
}

/// Empty repo_url + git_ref is 200 and still clears, not 400.
#[tokio::test]
async fn admin_patch_empty_url_with_git_ref_clears() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "develop").await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "repo_url": "", "git_ref": "other" })),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "empty URL + git_ref must clear, not 400: {out}"
    );
    let after = store
        .get_project_config(pid)
        .await
        .expect("config after empty+ref");
    assert!(
        after.git_remote().is_none(),
        "empty URL + git_ref must drop git_remote(): {:?}",
        after.repo_url
    );
}

/// Invalid git_ref after a bound URL is 400 and leaves the remote.
#[tokio::test]
async fn admin_patch_invalid_git_ref_is_400_leaves_remote() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "develop").await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "git_ref": "feat/../x" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "feat/../x: {out}");
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after feat/../x");
    let remote = still
        .git_remote()
        .expect("invalid git_ref must leave the remote bound");
    assert_eq!(remote.url, REPO);
    assert_eq!(remote.git_ref, "develop");

    let (st, out) = json_req(
        app.clone(),
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "git_ref": "has space" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "has space: {out}");
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after has space");
    let remote = still
        .git_remote()
        .expect("invalid git_ref must leave the remote bound");
    assert_eq!(remote.url, REPO);
    assert_eq!(remote.git_ref, "develop");

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "git_ref": "-bad" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "-bad: {out}");
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after -bad");
    let remote = still
        .git_remote()
        .expect("invalid git_ref must leave the remote bound");
    assert_eq!(remote.url, REPO);
    assert_eq!(remote.git_ref, "develop");
}

/// Credential userinfo URL is 400, does not echo the secret, leaves the remote.
#[tokio::test]
async fn admin_patch_userinfo_url_is_400_without_echoing_secret() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "develop").await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "repo_url": "https://user:unique-secret@example.test/acme.git" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "userinfo URL must 400: {out}");
    assert!(
        !out.to_string().contains("unique-secret"),
        "400 must not echo credentials: {out}"
    );
    let still = store
        .get_project_config(pid)
        .await
        .expect("config after userinfo 400");
    let remote = still
        .git_remote()
        .expect("userinfo URL must leave the remote bound");
    assert_eq!(remote.url, REPO);
    assert_eq!(remote.git_ref, "develop");
}

/// Whitespace-only repo_url is 200 and drops git_remote().
#[tokio::test]
async fn admin_patch_whitespace_url_clears() {
    let (app, store) = app_and_store().await;
    let pid = seed_project(&store).await;
    bind_remote(&store, pid, "develop").await;
    let cfg_uri = config_uri(pid);

    let (st, out) = json_req(
        app,
        "PATCH",
        &cfg_uri,
        Some(ADMIN),
        Some(json!({ "repo_url": "   " })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "whitespace URL: {out}");
    let after = store
        .get_project_config(pid)
        .await
        .expect("config after whitespace clear");
    assert!(
        after.git_remote().is_none(),
        "whitespace URL must drop git_remote(): {:?}",
        after.repo_url
    );
}
