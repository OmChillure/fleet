//! Live Slack / Notion / GitHub through `integration_query` and Grok tools.
//!
//! `#[ignore]` — needs the plane DB, encrypted tokens, and `grok login`.
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
use serde_json::{Value, json};
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

async fn live_state() -> (AppState, Uuid, Uuid) {
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
        connected.contains(&"slack") && connected.contains(&"notion"),
        "workspace {ws} missing slack/notion: {connected:?}"
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
        sandbox_backend: "stub".into(),
        stub_duration: Duration::from_millis(0),
        process_worker_argv: vec!["fleet-driver".into()],
        k8s: fleet_orchestrator::sandbox::K8sOpts::default(),
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

async fn tool(state: &AppState, ws: Uuid, user: Uuid, name: &str, args: Value) -> Value {
    let (content, action) = execute_tool(state, ws, user, name, &args, &llm()).await;
    assert!(action.ok, "{name} failed: {} {}", action.summary, content);
    content
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t1_list_and_get_connected_apps() {
    let (state, ws, user) = live_state().await;
    let listed = tool(
        &state,
        ws,
        user,
        "list_integrations",
        json!({ "connected_only": true }),
    )
    .await;
    let blob = listed.to_string();
    assert!(blob.contains("slack"), "{listed}");
    assert!(blob.contains("notion"), "{listed}");

    for provider in ["slack", "notion"] {
        let got = tool(
            &state,
            ws,
            user,
            "get_integration",
            json!({ "provider": provider }),
        )
        .await;
        assert!(
            got.get("connected").and_then(|v| v.as_bool()) == Some(true)
                || got.to_string().contains("connected"),
            "{provider}: {got}"
        );
        println!("T1 {provider} get_integration ok");
    }
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t2_slack_list_channels_and_users() {
    let (state, ws, user) = live_state().await;
    for op in ["list_channels", "list_users"] {
        let out = tool(
            &state,
            ws,
            user,
            "integration_query",
            json!({ "provider": "slack", "operation": op, "args": {} }),
        )
        .await;
        assert_eq!(out["ok"], true, "{op}: {out}");
        println!("T2 slack {op} ok summary={}", out["summary"]);
    }
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t3_notion_search() {
    let (state, ws, user) = live_state().await;
    let out = tool(
        &state,
        ws,
        user,
        "integration_query",
        json!({
            "provider": "notion",
            "operation": "search",
            "args": { "query": "" }
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "notion search: {out}");
    println!("T3 notion search ok summary={}", out["summary"]);
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t4_github_list_repos() {
    let (state, ws, user) = live_state().await;
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
    if action.ok && out["ok"] == true {
        println!("T4 github list_repos ok summary={}", out["summary"]);
        return;
    }
    let blob = out.to_string().to_lowercase();
    assert!(
        blob.contains("401")
            || blob.contains("credential")
            || blob.contains("not_connected")
            || blob.contains("not connected"),
        "github failed for an unexpected reason: {out}"
    );
    println!(
        "T4 github token is dead ({}) — reconnect GitHub",
        out["summary"]
    );
}

#[tokio::test]
#[ignore = "live OAuth tokens"]
async fn t5_jira_not_connected() {
    let (state, ws, user) = live_state().await;
    let (content, action) = execute_tool(
        &state,
        ws,
        user,
        "integration_query",
        &json!({
            "provider": "jira",
            "operation": "list_issues",
            "args": {}
        }),
        &llm(),
    )
    .await;
    assert!(
        !action.ok || content["ok"] == false,
        "jira should not be connected: {content}"
    );
    let blob = format!("{action:?}{content}");
    assert!(
        blob.to_lowercase().contains("not_connected")
            || blob.to_lowercase().contains("not connected")
            || blob.to_lowercase().contains("jira"),
        "{blob}"
    );
    println!("T5 jira not_connected ok");
}

#[tokio::test]
#[ignore = "live Grok + OAuth tokens"]
async fn l1_grok_chat_calls_slack_integration_query() {
    let (state, ws, user) = live_state().await;
    let model = fleet_server::grok::default_model();
    assert!(
        !model.is_empty(),
        "grok models empty — run grok login on this box"
    );
    let (used, text, actions) = fleet_server::grok::chat_with_tools(
        &state,
        ws,
        user,
        &state.grok,
        &model,
        &[(
            "user".into(),
            "List my Slack channels using the Slack integration. Do not guess.".into(),
        )],
        None,
        None,
        None,
    )
    .await
    .expect("grok chat_with_tools");
    println!("L1 model={used} actions={} text={}", actions.len(), text);
    let queried = actions.iter().any(|a| {
        a.tool == "integration_query" && a.ok && format!("{a:?}").to_lowercase().contains("slack")
    });
    assert!(
        queried,
        "expected a successful slack integration_query; actions={actions:?} text={text}"
    );
}
