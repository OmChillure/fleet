use crate::config::{AuthMode, Config};
use crate::orch::TickRunner;
use crate::sandbox::{build_runtime, RuntimeBuildOpts};
use crate::store::{connect, migrate, Store};
use crate::sync::LogBoardSync;
use anyhow::Context;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub tick: Arc<TickRunner>,
    pub admin_token: String,
    pub auth_mode: AuthMode,
}

pub async fn build_app_state(cfg: &Config) -> anyhow::Result<AppState> {
    let pool = connect(&cfg.database_url).await?;
    build_app_state_with_pool(cfg, pool).await
}

/// Reuse an existing pool (fleet-server already opened fleet-db).
pub async fn build_app_state_with_pool(cfg: &Config, pool: PgPool) -> anyhow::Result<AppState> {
    build_app_state_with_pool_io(cfg, pool, None).await
}

/// Same as [`build_app_state_with_pool`] plus a control-plane integration proxy
/// so workers can `use_integration` and finished runs can write back.
pub async fn build_app_state_with_pool_io(
    cfg: &Config,
    pool: PgPool,
    io: Option<std::sync::Arc<dyn crate::sandbox::io::IntegrationInvoker>>,
) -> anyhow::Result<AppState> {
    migrate(&pool).await?;
    let store = Store::new(pool);
    tokio::fs::create_dir_all(&cfg.data_dir)
        .await
        .with_context(|| format!("create DATA_DIR {}", cfg.data_dir.display()))?;
    let runtime = build_runtime(&RuntimeBuildOpts {
        backend: cfg.sandbox_backend.clone(),
        stub_duration: cfg.stub_duration,
        data_dir: cfg.data_dir.clone(),
        worker_argv: cfg.process_worker_argv.clone(),
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let orch_url = cfg.orchestrator_public_url.clone();
    // PR10: LogBoardSync adapter; per-project `auto_sync_external` gates tick calls.
    let board_sync = Arc::new(LogBoardSync);
    let mut runner = TickRunner::with_board_sync(
        store.clone(),
        runtime,
        board_sync,
        cfg.data_dir.clone(),
        orch_url,
    );
    if let Some(inv) = io {
        runner = runner.with_integrations(inv);
    }
    let tick = Arc::new(runner);

    Ok(AppState {
        store,
        tick,
        admin_token: cfg.admin_token.clone(),
        auth_mode: cfg.auth_mode,
    })
}
