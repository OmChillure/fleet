use crate::config::{AuthMode, Config};
use crate::orch::TickRunner;
use crate::sandbox::{build_runtime, RuntimeBuildOpts};
use crate::store::{connect, migrate, Store};
use crate::sync::LogBoardSync;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub tick: Arc<TickRunner>,
    pub admin_token: String,
    pub auth_mode: AuthMode,
}

pub async fn build_app_state(cfg: &Config) -> anyhow::Result<AppState> {
    build_app_state_from_parts(cfg).await
}

/// Build orchestrator state (used by standalone binary and fleet-server mount).
pub async fn build_app_state_from_parts(cfg: &Config) -> anyhow::Result<AppState> {
    let pool = connect(&cfg.database_url).await?;
    migrate(&pool).await?;
    let store = Store::new(pool);
    let _ = tokio::fs::create_dir_all(&cfg.data_dir).await;
    let runtime = build_runtime(&RuntimeBuildOpts {
        backend: cfg.sandbox_backend.clone(),
        stub_duration: cfg.stub_duration,
        data_dir: cfg.data_dir.clone(),
        worker_argv: cfg.process_worker_argv.clone(),
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let orch_url = if cfg.http_addr.starts_with(':') {
        format!("http://127.0.0.1{}", cfg.http_addr)
    } else if cfg.http_addr.starts_with("http") {
        cfg.http_addr.clone()
    } else {
        format!("http://{}", cfg.http_addr)
    };
    // PR10: LogBoardSync adapter; per-project `auto_sync_external` gates tick calls.
    let board_sync = Arc::new(LogBoardSync);
    let tick = Arc::new(TickRunner::with_board_sync(
        store.clone(),
        runtime,
        board_sync,
        cfg.data_dir.clone(),
        orch_url,
    ));

    Ok(AppState {
        store,
        tick,
        admin_token: cfg.admin_token.clone(),
        auth_mode: cfg.auth_mode,
    })
}
