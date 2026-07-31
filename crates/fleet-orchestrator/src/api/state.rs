use crate::config::{AuthMode, Config};
use crate::orch::TickRunner;
use crate::sandbox::build_runtime;
use crate::store::{connect, migrate, Store};
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
    migrate(&pool).await?;
    let store = Store::new(pool);
    let runtime =
        build_runtime(&cfg.sandbox_backend, cfg.stub_duration).map_err(|e| anyhow::anyhow!(e))?;
    let _ = tokio::fs::create_dir_all(&cfg.data_dir).await;
    let orch_url = if cfg.http_addr.starts_with(':') {
        format!("http://127.0.0.1{}", cfg.http_addr)
    } else if cfg.http_addr.starts_with("http") {
        cfg.http_addr.clone()
    } else {
        format!("http://{}", cfg.http_addr)
    };
    let tick = Arc::new(TickRunner::new(
        store.clone(),
        runtime,
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
