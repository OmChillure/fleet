//! fleet-orchestrator binary.
//!
//! MVP leadership: exactly one process per Postgres database.
//! Do not scale replicas against the same DB.

use std::net::SocketAddr;
use std::time::Duration;

use fleet_orchestrator::api::{build_app_state, router};
use fleet_orchestrator::config::Config;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .json()
        .init();

    let cfg = Config::from_env()?;
    let state = build_app_state(&cfg).await?;

    // Optional background tick loop for all projects is phase 2;
    // operators force ticks via POST /v1/projects/{id}/admin/tick.
    // Keep a wake channel future for multi-project loop later.
    let _tick_interval = cfg.tick_interval;

    let app = router(state);

    let addr: SocketAddr = if cfg.http_addr.starts_with(':') {
        format!("0.0.0.0{}", cfg.http_addr).parse()?
    } else {
        cfg.http_addr.parse().or_else(|_| {
            // host:port without scheme
            format!("{}", cfg.http_addr).parse()
        })?
    };

    info!(
        leadership_mode = %cfg.leadership_mode,
        %addr,
        tick_interval_s = cfg.tick_interval.as_secs(),
        sandbox_backend = %cfg.sandbox_backend,
        auth_mode = %cfg.auth_mode,
        data_dir = %cfg.data_dir.display(),
        note = "single_process_only_do_not_scale_replicas",
        "fleet-orchestrator listening"
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[allow(dead_code)]
fn _duration_marker() -> Duration {
    Duration::from_secs(1)
}
