//! Heartbeat + lease loop.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::client::{EffectiveModes, HostClient};
use crate::config::{LocalConfig, read_machine_token};
use crate::spawn;
use crate::update::{self, ApplyOptions, UpdateState, maybe_queue_auto};

pub async fn pair(url: &str, token: &str) -> Result<()> {
    let mut cfg = LocalConfig::load_or_default()?;
    let client = HostClient::for_pair(url, &cfg)?;
    let resp = client.pair(&cfg, token.trim()).await?;
    if resp.machine_token.trim().is_empty() {
        bail!("pair response missing machine_token");
    }
    let cp = resp
        .control_plane_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(url);
    cfg.control_plane_url = Some(normalize_url(cp));
    cfg.host_id = Some(resp.host_id);
    let config_path = cfg.save()?;
    let token_path = crate::config::write_machine_token(&resp.machine_token)?;
    println!("paired host_id={}", resp.host_id);
    if let Ok(mid) = crate::paths::load_or_create_machine_id() {
        println!("machine_id={mid}");
    }
    println!(
        "control_plane_url={}",
        cfg.control_plane_url.as_deref().unwrap_or("")
    );
    println!("wrote {}", config_path.display());
    println!("wrote {}", token_path.display());
    start_run_after_pair();
    Ok(())
}

/// Pair is a handshake. Offline after 90s unless `fleet-host run` is up.
fn start_run_after_pair() {
    let user = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "fleet-host"])
        .status();
    if matches!(user, Ok(s) if s.success()) {
        println!("started systemd user unit fleet-host");
        return;
    }
    let sys = std::process::Command::new("systemctl")
        .args(["enable", "--now", "fleet-host"])
        .status();
    if matches!(sys, Ok(s) if s.success()) {
        println!("started systemd unit fleet-host");
        return;
    }
    println!("this host goes offline after 90s without heartbeats");
    println!("start: fleet-host run");
    println!("   or: systemctl --user enable --now fleet-host");
}

pub fn status() -> Result<()> {
    let cfg = LocalConfig::load_or_default()?;
    let token = read_machine_token()?;
    let host_id = cfg
        .host_id
        .map(|u| u.to_string())
        .unwrap_or_else(|| "(not paired)".into());
    let url = cfg
        .control_plane_url
        .clone()
        .unwrap_or_else(|| "(none)".into());
    println!("host_id:        {host_id}");
    if let Ok(mid) = crate::paths::load_or_create_machine_id() {
        println!("machine_id:     {mid}");
    }
    println!("url:            {url}");
    println!("run_host:       {}", cfg.run_host);
    println!("run_container:  {}", cfg.run_container);
    println!("auto_update:    {}", cfg.auto_update);
    println!(
        "token:          {}",
        if token.is_some() {
            "present"
        } else {
            "missing"
        }
    );
    println!("config:         {}", crate::paths::config_path().display());
    Ok(())
}

pub fn unpair() -> Result<()> {
    let path = crate::paths::token_path();
    if crate::config::delete_machine_token()? {
        println!("removed {}", path.display());
    } else {
        println!("token already absent ({})", path.display());
    }
    println!("kept {}", crate::paths::config_path().display());
    Ok(())
}

pub async fn run() -> Result<()> {
    let cfg = LocalConfig::load_or_default()?;
    let token = read_machine_token()?.ok_or_else(|| {
        anyhow::anyhow!(
            "not paired: missing {} (run `fleet-host pair --url … --token …`)",
            crate::paths::token_path().display()
        )
    })?;
    let url = cfg.control_plane_url.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "config missing control_plane_url ({})",
            crate::paths::config_path().display()
        )
    })?;
    init_tracing(&cfg.log_level);
    info!(
        host_id = ?cfg.host_id,
        url = %url,
        run_host = cfg.run_host,
        run_container = cfg.run_container,
        "fleet-host run"
    );
    run_loop(cfg, url, token).await
}

/// Manual apply. Same path as auto; does not set CP `paused`/`drain`.
pub async fn update() -> Result<()> {
    let cfg = LocalConfig::load_or_default()?;
    init_tracing(&cfg.log_level);
    update::run_manual().await
}

pub fn init_tracing(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let lvl = match level.trim() {
            "error" | "warn" | "info" | "debug" | "trace" => level.trim(),
            _ => "info",
        };
        tracing_subscriber::EnvFilter::new(lvl)
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn run_loop(cfg: LocalConfig, url: String, token: String) -> Result<()> {
    let client = HostClient::with_machine_token(&url, token, &cfg)?;
    let jobs: Arc<Mutex<HashSet<Uuid>>> = Arc::new(Mutex::new(HashSet::new()));
    let effective = Arc::new(Mutex::new(EffectiveModes::from_local(&cfg)));
    let update_state = Arc::new(UpdateState::new());

    let mut hb = tokio::time::interval(cfg.heartbeat_interval());
    let mut poll = tokio::time::interval(cfg.poll_interval());
    hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = shutdown_signal() => {
                info!("shutdown: stop leasing (in-flight jobs keep running until they exit)");
                break;
            }
            _ = hb.tick() => {
                if update_state.exit_process.load(std::sync::atomic::Ordering::SeqCst) {
                    info!("exiting so the updater can replace fleet-host");
                    break;
                }
                let n = jobs.lock().await.len() as u32;
                match client.heartbeat(&cfg, n, None, update_state.is_updating()).await {
                    Ok(resp) => {
                        let mut eff = effective.lock().await;
                        if resp.effective.max_concurrent > 0 {
                            *eff = resp.effective;
                        } else {
                            eff.host = resp.effective.host;
                            eff.container = resp.effective.container;
                        }
                        if !resp.host.accepts_leases() {
                            warn!(status = ?resp.host.status, "host not accepting leases");
                        }
                        if let Some(desired) =
                            maybe_queue_auto(&update_state, &cfg, &resp.host, resp.desired.as_ref())
                        {
                            update_state.updating.store(
                                true,
                                std::sync::atomic::Ordering::SeqCst,
                            );
                            let client2 = client.clone();
                            let cfg2 = cfg.clone();
                            let jobs2 = jobs.clone();
                            let state2 = update_state.clone();
                            let n2 = n;
                            tokio::spawn(async move {
                                let _ = client2.heartbeat(&cfg2, n2, None, true).await;
                                let dest = match update::install_dir() {
                                    Ok(p) => p,
                                    Err(e) => {
                                        warn!(error = %e, "update: no install dir");
                                        state2.finish();
                                        return;
                                    }
                                };
                                let result = update::apply_desired(
                                    &client2,
                                    &cfg2,
                                    &desired,
                                    &dest,
                                    &jobs2,
                                    &state2,
                                    &ApplyOptions::production(),
                                )
                                .await;
                                state2.finish();
                                match result {
                                    Ok(update::ApplyOutcome::Noop)
                                    | Ok(update::ApplyOutcome::QuietNoPrebuilt) => {
                                        state2.updating.store(
                                            false,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                    }
                                    Ok(update::ApplyOutcome::ChildrenSwapped) => {
                                        info!("applied child-bin update");
                                    }
                                    Ok(update::ApplyOutcome::HostRestartPending) => {
                                        info!("host-bin update staged");
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "auto-update apply failed");
                                        state2.updating.store(
                                            false,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                        state2.stop_claiming.store(
                                            false,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if e.downcast_ref::<crate::client::ApiError>()
                            .is_some_and(|a| matches!(a, crate::client::ApiError::Auth(_)))
                            || msg.contains("machine token rejected")
                        {
                            return Err(e);
                        }
                        warn!(error = %e, "heartbeat failed");
                    }
                }
            }
            _ = poll.tick() => {
                if update_state.exit_process.load(std::sync::atomic::Ordering::SeqCst) {
                    info!("exiting so the updater can replace fleet-host");
                    break;
                }
                if update_state
                    .stop_claiming
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    continue;
                }
                let n = jobs.lock().await.len() as u32;
                let eff = effective.lock().await.clone();
                let max = eff.max_concurrent.min(cfg.local_max_jobs());
                if max == 0 || n >= max {
                    continue;
                }
                match client.lease().await {
                    Ok(Some(claim)) => {
                        let claim_id = claim.id;
                        jobs.lock().await.insert(claim_id);
                        let jobs2 = jobs.clone();
                        let client2 = client.clone();
                        let cfg2 = cfg.clone();
                        let eff_host = eff.host;
                        let eff_ctr = eff.container;
                        tokio::spawn(async move {
                            let outcome = spawn::run_claim(
                                &cfg2,
                                &claim.payload,
                                eff_host,
                                eff_ctr,
                            )
                            .await;
                            let (ok, err) = match outcome {
                                Ok(()) => (true, None),
                                Err(e) => {
                                    error!(claim = %claim_id, error = %e, "job failed");
                                    (false, Some(e.to_string()))
                                }
                            };
                            if let Err(e) = client2
                                .complete(claim_id, &claim.lease_token, ok, err.as_deref())
                                .await
                            {
                                error!(claim = %claim_id, error = %e, "complete failed");
                            }
                            jobs2.lock().await.remove(&claim_id);
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let msg = e.to_string();
                        if e.downcast_ref::<crate::client::ApiError>()
                            .is_some_and(|a| matches!(a, crate::client::ApiError::Auth(_)))
                            || msg.contains("machine token rejected")
                        {
                            return Err(e);
                        }
                        warn!(error = %e, "lease poll failed");
                    }
                }
            }
        }
    }

    // Drain in-flight children so complete POSTs can finish.
    let drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !jobs.lock().await.is_empty() {
        if tokio::time::Instant::now() >= drain_deadline {
            warn!(
                left = jobs.lock().await.len(),
                "shutdown with jobs still running"
            );
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub fn print_usage() {
    eprintln!(
        "\
fleet-host — paired worker host

Usage:
  fleet-host pair --url URL --token TOKEN
  fleet-host run
  fleet-host update
  fleet-host status
  fleet-host unpair

Config:  {}
Token:   {}
",
        crate::paths::config_path().display(),
        crate::paths::token_path().display()
    );
}
