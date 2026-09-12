//! Unified Agent Fleet HTTP API.
//!
//! Auth + workspaces + integrations + orchestrator control plane on one process.

use std::net::SocketAddr;

use fleet_auth::validate_cookie_policy;
use fleet_db::connect_and_migrate;
use fleet_orchestrator::config::Config as OrchConfig;
use fleet_server::{AppState, ServerConfig, build_http_app};
use tracing_subscriber::EnvFilter;

/// Default when `RUST_LOG` is unset.
///
/// Override with standard `RUST_LOG` — levels stack as usual:
/// - `info`  — normal ops (startup, HTTP, auth success) + warn + error
/// - `debug` — above + detailed request/JWT/query noise
/// - `warn`  — only warnings and errors (quiet)
const DEFAULT_RUST_LOG: &str = "info,sqlx=warn,tower_http=info";

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    init_tracing();

    if let Err(err) = run().await {
        // Print full anyhow chain so startup failures are never silent.
        tracing::error!(error = %err, "fleet-server failed to start");
        for (i, cause) in err.chain().enumerate().skip(1) {
            tracing::error!(index = i, cause = %cause, "caused by");
        }
        // Also to stderr in case the subscriber filter hid anything.
        eprintln!("fleet-server fatal: {err:#}");
        std::process::exit(1);
    }
}

fn warn_stale_worker_dist() {
    let dest = std::env::var("FLEET_WORKER_DIST_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".tmp/vms/dist".into());
    let want_host = fleet_server::package_versions::fleet_host_version();
    let want_driver = fleet_server::package_versions::fleet_driver_version();
    let root = std::path::Path::new(&dest);
    if !root.is_dir() {
        tracing::warn!(
            dest,
            "no staged worker dist; paired runners cannot auto-update. Run scripts/stage-worker-dist.sh"
        );
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for ent in rd.flatten() {
        let manifest = ent.path().join("manifest.json");
        if !manifest.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let parsed =
            serde_json::from_str::<serde_json::Value>(&raw).unwrap_or(serde_json::Value::Null);
        let bins = parsed.get("bins");
        let host_ver = staged_bin_version(bins, "fleet-host");
        let driver_ver = staged_bin_version(bins, "fleet-driver");
        let host_ok = !want_host.is_empty() && host_ver.as_deref() == Some(want_host);
        let driver_ok = !want_driver.is_empty() && driver_ver.as_deref() == Some(want_driver);
        if host_ok && driver_ok {
            continue;
        }
        let ahead = host_ver
            .as_deref()
            .is_some_and(|v| fleet_server::package_versions::version_is_ahead(v, want_host))
            || driver_ver
                .as_deref()
                .is_some_and(|v| fleet_server::package_versions::version_is_ahead(v, want_driver));
        if ahead {
            tracing::warn!(
                platform = %ent.file_name().to_string_lossy(),
                staged_host = %host_ver.as_deref().unwrap_or(""),
                staged_driver = %driver_ver.as_deref().unwrap_or(""),
                want_host,
                want_driver,
                "staged package is newer than the verification version — bump [verify] in runner-packages.toml"
            );
        } else {
            tracing::warn!(
                platform = %ent.file_name().to_string_lossy(),
                staged_host = %host_ver.as_deref().unwrap_or(""),
                staged_driver = %driver_ver.as_deref().unwrap_or(""),
                want_host,
                want_driver,
                "outdated worker package — restage with scripts/stage-worker-dist.sh before remote auto-update"
            );
        }
    }
}

fn staged_bin_version(bins: Option<&serde_json::Value>, stem: &str) -> Option<String> {
    let obj = bins?.as_object()?;
    let exe = format!("{stem}.exe");
    let v = obj.get(stem).or_else(|| obj.get(exe.as_str()))?;
    let s = v.get("version")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_RUST_LOG)),
        )
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .init();
}

async fn run() -> anyhow::Result<()> {
    tracing::info!("loading configuration…");
    warn_stale_worker_dist();
    let config = ServerConfig::from_env().map_err(|e| {
        tracing::error!(error = %e, "configuration error");
        e
    })?;
    validate_cookie_policy(&config.auth);
    fleet_auth::validate_signup_policy(
        config.auth.signup_token.as_deref(),
        &[
            config.integrations.api_public_base.as_str(),
            config.integrations.ui_public_base.as_str(),
        ],
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    let oauth_configured: Vec<&str> = [
        ("github", config.integrations.github.is_some()),
        ("gitlab", config.integrations.gitlab.is_some()),
        ("slack", config.integrations.slack.is_some()),
        ("linear", config.integrations.linear.is_some()),
        ("jira", config.integrations.jira.is_some()),
        ("figma", config.integrations.figma.is_some()),
        ("notion", config.integrations.notion.is_some()),
        ("google_drive", config.integrations.google_drive.is_some()),
        ("teams", config.integrations.teams.is_some()),
        ("discord", config.integrations.discord.is_some()),
        ("trello", config.integrations.trello.is_some()),
        ("asana", config.integrations.asana.is_some()),
    ]
    .into_iter()
    .filter_map(|(id, ok)| ok.then_some(id))
    .collect();

    tracing::info!(
        bind = %config.bind,
        cors_origins = ?config.cors_origins,
        api_public_base = %config.integrations.api_public_base,
        ui_public_base = %config.integrations.ui_public_base,
        oauth_configured = ?oauth_configured,
        ollama_enabled = config.ollama.enabled,
        ollama_base = %config.ollama.base_url,
        ollama_model = ?config.ollama.default_model,
        grok_enabled = config.grok.enabled,
        grok_auth = %config.grok.auth_path.display(),
        grok_api_key_fallback = config.grok.api_key.is_some(),
        "configuration loaded"
    );

    let db = connect_and_migrate(&config.database_url)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "database startup failed");
            anyhow::anyhow!(e)
        })?;
    tracing::info!("database ready");

    let orch_cfg = OrchConfig::from_env_for_server(&config.database_url, &config.bind.to_string())
        .map_err(|e| {
            tracing::error!(error = %e, "orchestrator config failed");
            e
        })?;
    let io: std::sync::Arc<dyn fleet_orchestrator::sandbox::io::IntegrationInvoker> =
        std::sync::Arc::new(fleet_server::work_io::ControlPlaneIo {
            db: db.clone(),
            key: config.integrations.credentials_key.clone(),
        });
    let orch =
        fleet_orchestrator::api::build_app_state_with_pool_io(&orch_cfg, db.clone(), Some(io))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "orchestrator state init failed");
                e
            })?;
    tracing::info!("orchestrator state ready");

    // Local drain loop: queue thousands of tasks, run FLEET_MAX_AGENTS at a time (no k8s).
    {
        let orch_bg = orch.clone();
        let interval = orch_cfg.tick_interval;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match orch_bg.store.list_open_project_ids().await {
                    Ok(ids) if ids.is_empty() => {}
                    Ok(ids) => {
                        for id in ids {
                            if let Err(e) = orch_bg.tick.tick_project(id).await {
                                tracing::warn!(project_id = %id, error = %e, "background tick failed");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "background tick: list open projects failed");
                    }
                }
            }
        });
        tracing::info!(
            tick_interval_s = interval.as_secs(),
            "background fleet ticker started (local sandbox queue)"
        );
    }

    let state = AppState {
        db,
        auth: config.auth.clone(),
        integrations: config.integrations.clone(),
        orch: orch.clone(),
        ollama: config.ollama.clone(),
        grok: config.grok.clone(),
        openai: config.openai.clone(),
        claude: config.claude.clone(),
        gemini: config.gemini.clone(),
        worker_dist_dir: config.worker_dist_dir.clone(),
    };

    {
        let case_state = state.clone();
        let interval = fleet_server::cases::wake_interval();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                fleet_server::cases::process_due(&case_state, 20).await;
            }
        });
        tracing::info!(
            interval_s = interval.as_secs(),
            "background case wake loop started (CASE_AUTO_DECIDE, CASE_WAKE_INTERVAL_S)"
        );
    }

    {
        let ticket_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(fleet_server::tickets::POLL_MIN_INTERVAL).await;
                fleet_server::tickets::poll_due_workspaces(&ticket_state).await;
            }
        });
        tracing::info!("background ticket inbox poller started");
    }

    fleet_server::bus::start_bus_tasks(state.db.clone());

    let app = build_http_app(state, orch, &config)?;

    let addr: SocketAddr = config.bind;
    tracing::info!(
        %addr,
        "fleet-server listening (auth + workspaces + integrations + fleet + orchestrator)"
    );
    tracing::info!("logging: RUST_LOG=info (default) | debug (verbose) | warn (quiet errors only)");

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        tracing::error!(%addr, error = %e, "failed to bind listen address");
        e
    })?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "server exited with error");
        e
    })?;
    Ok(())
}
