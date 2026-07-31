//! fleet-orchestrator binary.
//!
//! MVP leadership: exactly one process per Postgres database.
//! Do not scale replicas against the same DB.

use fleet_orchestrator::config::Config;
use fleet_orchestrator::sandbox::build_runtime;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .json()
        .init();

    let cfg = Config::from_env()?;
    let _runtime = build_runtime(&cfg.sandbox_backend, cfg.stub_duration)
        .map_err(|e| anyhow::anyhow!(e))?;

    info!(
        leadership_mode = %cfg.leadership_mode,
        http_addr = %cfg.http_addr,
        tick_interval_s = cfg.tick_interval.as_secs(),
        sandbox_backend = %cfg.sandbox_backend,
        auth_mode = %cfg.auth_mode,
        data_dir = %cfg.data_dir.display(),
        note = "single_process_only_do_not_scale_replicas",
        "fleet-orchestrator starting"
    );

    info!("bootstrap complete; reconcile loop not yet enabled (PR5)");
    Ok(())
}
