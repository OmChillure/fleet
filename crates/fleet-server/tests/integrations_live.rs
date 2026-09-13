//! Live Notion / GitHub through `integration_query`.
//!
//! `#[ignore]` — needs the plane DB and encrypted tokens.
//!
//!   FLEET_LIVE_INTEGRATIONS=1 cargo test -p fleet-server --test integrations_live -- --ignored --nocapture

use std::env;
use std::time::Duration;

use fleet_db::repos::integrations as integ_repo;
use fleet_db::{PoolConfig, connect_and_migrate_with};
use fleet_orchestrator::api::build_app_state_with_pool_io;
use fleet_orchestrator::config::{AuthMode, Config as OrchConfig};
use fleet_server::chat_tools::{ChatLlmContext, execute_tool};
use fleet_server::work_io::ControlPlaneIo;
use fleet_server::{AppState, ServerConfig};
use serde_json::json;
use uuid::Uuid;

const DEFAULT_WS: &str = "01a03929-e0ad-7dd3-9835-71c9c4f5df01";

fn require_live() {
    let _ = dotenvy::dotenv();
    assert_eq!(
        env::var("FLEET_LIVE_INTEGRATIONS").ok().as_deref(),
        Some("1"),
        "set FLEET_LIVE_INTEGRATIONS=1"
    );
}

async fn live_state_requiring(need: &[&str]) -> (AppState, Uuid, Uuid) {
    require_live();
    let config = ServerConfig::from_env().expect("ServerConfig::from_env");
    let db = connect_and_migrate_with(
        &config.database_url,
        &PoolConfig {
            max_connections: 5,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(30)),
            max_lifetime: None,
        },
    )
    .await
    .expect("db");

    let ws = env::var("FLEET_LIVE_WORKSPACE_ID")
        .ok()
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(|| Uuid::parse_str(DEFAULT_WS).unwrap());
    let rows = integ_repo::list_for_workspace(&db, ws)
        .await
        .expect("list connections");
    let connected: Vec<_> = rows
        .iter()
        .filter(|r| r.status == "connected")
        .map(|r| r.provider.as_str())
        .collect();
    assert!(
        need.iter().all(|p| connected.contains(p)),
        "workspace {ws} missing {need:?}: {connected:?}"
    );
    let user = rows
        .iter()
        .find_map(|r| r.connected_by)
        .expect("connected_by");

    let orch_cfg = OrchConfig {
        database_url: config.database_url.clone(),
        admin_token: "live-integrations".into(),
        auth_mode: AuthMode::Token,
        http_addr: "127.0.0.1:0".into(),
        orchestrator_public_url: "http://127.0.0.1:0".into(),
        data_dir: std::env::temp_dir().join("fleet-integrations-live"),
        tick_interval: Duration::from_secs(30),
        max_agents: 1,
        heartbeat_stale: Duration::from_secs(60),
        spawn_timeout: Duration::from_secs(30),
        kill_grace: Duration::from_secs(0),
        default_task_timeout: Duration::from_secs(3600),
        default_max_retries: 1,
        retry_backoff: Duration::from_secs(10),
        on_task_fail: "fail_run".into(),
        auto_sync_external: false,
        lock_wait_timeout: Duration::from_secs(5),
        log_level: "info".into(),
        leadership_mode: "single_process".into(),
        project_filter: None,
    };
    let io: std::sync::Arc<dyn fleet_orchestrator::sandbox::io::IntegrationInvoker> =
        std::sync::Arc::new(ControlPlaneIo {
            db: db.clone(),
            key: config.integrations.credentials_key.clone(),
        });
    let orch = build_app_state_with_pool_io(&orch_cfg, db.clone(), Some(io))
        .await
        .expect("orch");

    let state = AppState {
        db,
        auth: config.auth.clone(),
        integrations: config.integrations.clone(),
        orch,
        ollama: config.ollama.clone(),
        grok: config.grok.clone(),
        openai: config.openai.clone(),
        claude: config.claude.clone(),
        gemini: config.gemini.clone(),
        worker_dist_dir: config.worker_dist_dir.clone(),
    };
    (state, ws, user)
}

fn llm() -> ChatLlmContext {
    ChatLlmContext {
        provider: "grok".into(),
        model: "grok-4.6".into(),
        runner_id: None,
        effort: None,
    }
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t3_notion_search() {
    let (state, ws, user) = live_state_requiring(&["notion"]).await;
    let (out, action) = execute_tool(
        &state,
        ws,
        user,
        "integration_query",
        &json!({
            "provider": "notion",
            "operation": "search",
            "args": { "query": "" }
        }),
        &llm(),
    )
    .await;
    assert!(action.ok, "notion search failed: {} {out}", action.summary);
    assert_eq!(out["ok"], true, "notion search: {out}");
    assert_eq!(out["provider"], "notion", "provider: {out}");
    assert_eq!(out["operation"], "search", "operation: {out}");
    let items = out["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("data.items: {out}"));
    let count = out["data"]["count"].as_u64().unwrap_or(0);
    assert!(
        count >= 1 && !items.is_empty(),
        "notion search must map at least one item (empty is fail-closed): {out}"
    );
    assert_eq!(count as usize, items.len(), "count must match items: {out}");
    let item = &items[0];
    assert!(
        item.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some(),
        "items[0].id: {out}"
    );
    assert!(
        item.get("object").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some(),
        "items[0].object: {out}"
    );
    assert!(
        item.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some(),
        "items[0].url: {out}"
    );
    assert!(item.get("title").is_some(), "items[0].title: {out}");
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t4_github_list_repos() {
    let (state, ws, user) = live_state_requiring(&["github"]).await;
    let (out, action) = execute_tool(
        &state,
        ws,
        user,
        "integration_query",
        &json!({
            "provider": "github",
            "operation": "list_repos",
            "args": {}
        }),
        &llm(),
    )
    .await;
    assert!(
        action.ok,
        "github list_repos failed (token_expired/not_connected is fail-closed): {} {out}",
        action.summary
    );
    assert_eq!(out["ok"], true, "github list_repos: {out}");
    assert_eq!(out["provider"], "github", "provider: {out}");
    assert_eq!(out["operation"], "list_repos", "operation: {out}");
    let items = out["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("data.items: {out}"));
    let count = out["data"]["count"].as_u64().unwrap_or(0);
    assert!(
        count >= 1 && !items.is_empty(),
        "github list_repos must map at least one item (empty is fail-closed): {out}"
    );
    assert_eq!(count as usize, items.len(), "count must match items: {out}");
    assert!(
        items[0]
            .get("full_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some(),
        "items[0].full_name (poll_github reads this): {out}"
    );
}
