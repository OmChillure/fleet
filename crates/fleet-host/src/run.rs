//! Heartbeat + lease loop.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::client::{BootstrapPending, EffectiveModes, HostClient, InstallAck, PairResponse};
use crate::config::{LocalConfig, read_machine_token};
use crate::spawn;
use crate::update::{self, ApplyOptions, UpdateState, maybe_queue_auto};

static INSTALL_ACK: std::sync::Mutex<Option<InstallAck>> = std::sync::Mutex::new(None);
static INSTALLING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn take_install_ack() -> Option<InstallAck> {
    INSTALL_ACK.lock().ok().and_then(|mut g| g.take())
}

fn queue_install(spec: String) {
    let mut guard = match INSTALLING.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.as_deref() == Some(spec.as_str()) {
        return;
    }
    if guard.is_some() {
        return;
    }
    *guard = Some(spec.clone());
    drop(guard);
    std::thread::spawn(move || {
        let result = crate::agents::ollama_pull(&spec);
        if let Ok(mut g) = INSTALL_ACK.lock() {
            *g = Some(match result {
                Ok(()) => InstallAck {
                    spec: spec.clone(),
                    ok: true,
                    error: None,
                },
                Err(e) => InstallAck {
                    spec: spec.clone(),
                    ok: false,
                    error: Some(e),
                },
            });
        }
        if let Ok(mut g) = INSTALLING.lock() {
            *g = None;
        }
        crate::agents::invalidate_probe_cache();
    });
}

const CONTROL_PLANE_URL_ENV: &str = "FLEET_CONTROL_PLANE_URL";
const PAIR_TOKEN_ENV: &str = "FLEET_PAIR_TOKEN";
const BOOTSTRAP_SECRET_ENV: &str = "FLEET_BOOTSTRAP_SECRET";

pub async fn pair(url: &str, token: &str, force: bool) -> Result<()> {
    let mut cfg = LocalConfig::load_or_default()?;
    refuse_url_switch(&cfg, url, force)?;
    let client = HostClient::for_pair(url, &cfg)?;
    let resp = client.pair(&cfg, token.trim()).await?;
    persist_pair(&mut cfg, url, &resp)?;
    start_run_after_pair();
    Ok(())
}

fn refuse_url_switch(cfg: &LocalConfig, url: &str, force: bool) -> Result<()> {
    let Some(existing) = cfg
        .control_plane_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let have_token = crate::config::read_machine_token()?.is_some();
    if !have_token {
        return Ok(());
    }
    let old = normalize_url(existing);
    let new = normalize_url(url);
    if old != new && !force {
        bail!(
            "already paired to {old}; pass --force to pair {new} (this rotates the machine token)"
        );
    }
    Ok(())
}

fn persist_pair(cfg: &mut LocalConfig, url: &str, resp: &PairResponse) -> Result<()> {
    if resp.machine_token.trim().is_empty() {
        bail!("pair response missing machine_token");
    }
    let advertised = resp
        .control_plane_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(url);
    // In-cluster bootstrap: API public base is often localhost. Keep the
    // locator this process used to reach the plane.
    let cp = if is_unusable_bind_url(advertised) && !is_unusable_bind_url(url) {
        url
    } else if is_loopback_url(advertised) && !is_loopback_url(url) {
        url
    } else {
        advertised
    };
    if is_unusable_bind_url(cp) {
        bail!("control plane URL is not reachable (bind address {cp})");
    }
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
    if let Some((ok, detail)) = read_last_heartbeat() {
        println!(
            "last_heartbeat: {} {}",
            if ok { "ok" } else { "error" },
            detail
        );
    } else {
        println!("last_heartbeat: (none)");
    }
    Ok(())
}

fn last_heartbeat_path() -> std::path::PathBuf {
    crate::paths::last_heartbeat_path()
}

fn record_last_heartbeat(ok: bool, detail: &str) {
    let body = serde_json::json!({
        "ok": ok,
        "detail": detail.chars().take(240).collect::<String>(),
        "at": chrono::Utc::now().to_rfc3339(),
    });
    let _ = std::fs::write(last_heartbeat_path(), body.to_string());
}

fn read_last_heartbeat() -> Option<(bool, String)> {
    let raw = std::fs::read_to_string(last_heartbeat_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let ok = v.get("ok")?.as_bool()?;
    let detail = v
        .get("detail")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    Some((ok, detail))
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

/// One `fleet-host run` per config dir. A second process would share the
/// machine token and pick docker vs kube nondeterministically.
pub fn acquire_run_lock() -> Result<crate::paths::RunLock> {
    crate::paths::acquire_run_lock()
}

pub async fn run() -> Result<()> {
    let cfg = LocalConfig::load_or_default()?;
    init_tracing(&cfg.log_level);
    let _run_lock = acquire_run_lock()?;
    let (cfg, url, token) = ensure_paired(cfg).await?;
    info!(
        host_id = ?cfg.host_id,
        url = %url,
        run_host = cfg.run_host,
        run_container = cfg.run_container,
        "fleet-host run"
    );
    run_loop(cfg, url, token).await
}

async fn ensure_paired(mut cfg: LocalConfig) -> Result<(LocalConfig, String, String)> {
    if let (Some(token), Some(url)) = (read_machine_token()?, cfg.control_plane_url.clone()) {
        return Ok((cfg, url, token));
    }
    let url = cfg
        .control_plane_url
        .clone()
        .or_else(|| env_nonempty(CONTROL_PLANE_URL_ENV))
        .ok_or_else(|| {
            anyhow!(
                "not paired: set {CONTROL_PLANE_URL_ENV} or run `fleet-host pair --url … --token …`"
            )
        })?;
    if let Some(pair_token) = env_nonempty(PAIR_TOKEN_ENV) {
        let client = HostClient::for_pair(&url, &cfg)?;
        let resp = client.pair(&cfg, pair_token.trim()).await?;
        persist_pair(&mut cfg, &url, &resp)?;
        let token = resp.machine_token;
        let url = cfg.control_plane_url.clone().unwrap_or(url);
        return Ok((cfg, url, token));
    }
    if let Some(secret) = env_nonempty(BOOTSTRAP_SECRET_ENV) {
        let client = HostClient::for_pair(&url, &cfg)?;
        let resp = bootstrap_until_workspace(&client, &cfg, &secret).await?;
        persist_pair(&mut cfg, &url, &resp)?;
        let token = resp.machine_token;
        let url = cfg.control_plane_url.clone().unwrap_or(url);
        return Ok((cfg, url, token));
    }
    bail!(
        "not paired: missing {} (run `fleet-host pair --url … --token …`)",
        crate::paths::token_path().display()
    )
}

async fn bootstrap_until_workspace(
    client: &HostClient,
    cfg: &LocalConfig,
    secret: &str,
) -> Result<PairResponse> {
    let mut warned = false;
    loop {
        match client.bootstrap(cfg, secret).await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.downcast_ref::<BootstrapPending>().is_some() || is_no_workspace(&e) => {
                if !warned {
                    info!("bootstrap waiting for a workspace");
                    warned = true;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => {
                warn!(error = %e, "bootstrap failed; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

fn is_no_workspace(err: &anyhow::Error) -> bool {
    err.to_string().contains("no_workspace")
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_machine_auth_err(e: &anyhow::Error) -> bool {
    e.downcast_ref::<crate::client::ApiError>()
        .is_some_and(|a| matches!(a, crate::client::ApiError::Auth(_)))
        || e.to_string().contains("machine token rejected")
}

/// Same `machine.id`, new `machine.token`. Does not wipe the id file.
/// Needs `FLEET_BOOTSTRAP_SECRET` (fleet-dev sets it). Otherwise the human
/// runs `fleet-host pair` — do not invent a token.
async fn reclaim_machine_token(cfg: &mut LocalConfig, url: &str) -> Result<HostClient> {
    let secret = env_nonempty(BOOTSTRAP_SECRET_ENV)
        .ok_or_else(|| anyhow!("machine token rejected; re-pair with `fleet-host pair`"))?;
    warn!("machine token rejected; reclaiming via bootstrap (same machine.id)");
    let pair_client = HostClient::for_pair(url, cfg)?;
    let resp = bootstrap_until_workspace(&pair_client, cfg, &secret).await?;
    persist_pair(cfg, url, &resp)?;
    HostClient::with_machine_token(url, &resp.machine_token, cfg)
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

async fn run_loop(mut cfg: LocalConfig, url: String, token: String) -> Result<()> {
    cfg.apply_ollama_models_env();
    let bus = crate::bus::BusHandle::from_env().await;
    let mut client = HostClient::with_machine_token(&url, token, &cfg)?;
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
                match client.heartbeat(&cfg, n, None, update_state.is_updating(), take_install_ack()).await {
                    Ok(resp) => {
                        record_last_heartbeat(true, "ok");
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
                        let mut persist = cfg
                            .sync_auto_update_from_plane(resp.host.auto_update_or_default());
                        if persist {
                            info!(
                                auto_update = cfg.auto_update,
                                "synced auto_update from control plane"
                            );
                        }
                        if let Some(rs) = resp.host.runtime_settings.as_ref() {
                            if cfg.apply_runtime_settings(rs) {
                                persist = true;
                                info!("applied runtime_settings from control plane");
                            }
                        }
                        if persist {
                            if let Err(e) = cfg.save() {
                                warn!(error = %e, "failed to persist control-plane host settings");
                            }
                        }
                        if let Some(spec) = resp.install.as_ref().and_then(|v| {
                            let provider = v.get("provider").and_then(|p| p.as_str()).unwrap_or("");
                            let spec = v.get("spec").and_then(|s| s.as_str()).unwrap_or("").trim();
                            if provider == "ollama" && !spec.is_empty() {
                                Some(spec.to_string())
                            } else {
                                None
                            }
                        }) {
                            queue_install(spec);
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
                                let _ = client2.heartbeat(&cfg2, n2, None, true, None).await;
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
                        record_last_heartbeat(false, &e.to_string());
                        if is_machine_auth_err(&e) {
                            match reclaim_machine_token(&mut cfg, &url).await {
                                Ok(c) => client = c,
                                Err(re) => return Err(re),
                            }
                            continue;
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
                        let bus2 = bus.clone();
                        let kind = claim
                            .payload
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let session_id = claim
                            .payload
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .and_then(|s| Uuid::parse_str(s).ok())
                            .unwrap_or(Uuid::nil());
                        let signal = kind == "signal_session";
                        let chat = kind == "chat_turn";
                        let eff_host = eff.host;
                        let eff_ctr = eff.container;
                        tokio::spawn(async move {
                            if !signal && !chat {
                                bus2.session_started(session_id).await;
                            }
                            let mut chat_result = None;
                            let outcome = if signal {
                                spawn::kill_session(session_id).await
                            } else if chat {
                                match tokio::task::spawn_blocking({
                                    let payload = claim.payload.clone();
                                    move || crate::chat_cli::run_chat_turn(&payload)
                                })
                                .await
                                {
                                    Ok(Ok(v)) => {
                                        chat_result = Some(v);
                                        Ok(())
                                    }
                                    Ok(Err(e)) => Err(e),
                                    Err(e) => Err(anyhow!(e.to_string())),
                                }
                            } else {
                                match serde_json::from_value::<crate::client::SpawnPayload>(
                                    claim.payload.clone(),
                                ) {
                                    Ok(payload) => {
                                        spawn::run_claim(&cfg2, &payload, eff_host, eff_ctr).await
                                    }
                                    Err(e) => Err(anyhow!("invalid spawn payload: {e}")),
                                }
                            };
                            let (ok, err) = match outcome {
                                Ok(()) => {
                                    if !signal && !chat {
                                        if let Ok(payload) =
                                            serde_json::from_value::<crate::client::SpawnPayload>(
                                                claim.payload.clone(),
                                            )
                                        {
                                            let dir = cfg2.session_dir(session_id);
                                            if let Err(e) =
                                                spawn::publish_guest_proof(&payload, &dir).await
                                            {
                                                warn!(
                                                    claim = %claim_id,
                                                    error = %e,
                                                    "publish guest proof failed"
                                                );
                                            }
                                        }
                                    }
                                    (true, None)
                                }
                                Err(e) => {
                                    error!(claim = %claim_id, error = %e, "job failed");
                                    if chat {
                                        chat_result = Some(serde_json::json!({
                                            "ok": false,
                                            "error": e.to_string(),
                                        }));
                                    }
                                    (false, Some(e.to_string()))
                                }
                            };
                            if signal {
                                bus2.session_cancelled(session_id).await;
                            } else if !chat {
                                bus2.session_finished(session_id, ok, err.as_deref()).await;
                            }
                            if let Err(e) = client2
                                .complete(
                                    claim_id,
                                    &claim.lease_token,
                                    ok,
                                    err.as_deref(),
                                    chat_result,
                                )
                                .await
                            {
                                error!(claim = %claim_id, error = %e, "complete failed");
                            }
                            jobs2.lock().await.remove(&claim_id);
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if is_machine_auth_err(&e) {
                            match reclaim_machine_token(&mut cfg, &url).await {
                                Ok(c) => client = c,
                                Err(re) => return Err(re),
                            }
                            continue;
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

fn host_from_url(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let rest = rest.split('/').next().unwrap_or(rest);
    if let Some(inside) = rest.strip_prefix('[') {
        return inside.split(']').next().unwrap_or("");
    }
    if rest.starts_with("::") {
        return rest.split('%').next().unwrap_or(rest);
    }
    rest.rsplit_once(':').map(|(h, _)| h).unwrap_or(rest)
}

pub(crate) fn is_loopback_url(url: &str) -> bool {
    agent_fleet_contracts::url_is_loopback(url)
}

fn is_unusable_bind_url(url: &str) -> bool {
    let host = host_from_url(url).trim_matches(|c| c == '[' || c == ']');
    matches!(host, "0.0.0.0" | "::" | "")
}

pub fn print_usage() {
    eprintln!(
        "\
fleet-host — paired worker host

Usage:
  fleet-host pair --url URL --token TOKEN [--force]
  fleet-host run
  fleet-host update
  fleet-host status
  fleet-host unpair
  fleet-host --version

Config:  {}
Token:   {}
",
        crate::paths::config_path().display(),
        crate::paths::token_path().display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PairResponse;
    use std::sync::Mutex;
    use uuid::Uuid;

    static CONFIG_DIR_LOCK: Mutex<()> = Mutex::new(());

    fn pair_resp(control_plane_url: &str) -> PairResponse {
        PairResponse {
            host_id: Uuid::now_v7(),
            machine_token: "flm_testtoken".into(),
            workspace_id: None,
            control_plane_url: Some(control_plane_url.into()),
            cp_settings: serde_json::Value::Null,
        }
    }

    #[test]
    fn persist_pair_keeps_non_loopback_url_when_response_is_localhost() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        persist_pair(
            &mut cfg,
            "http://api.agent-fleet.svc.cluster.local:8080",
            &pair_resp("http://localhost:8080"),
        )
        .unwrap();
        assert_eq!(
            cfg.control_plane_url.as_deref(),
            Some("http://api.agent-fleet.svc.cluster.local:8080")
        );
    }

    #[test]
    fn persist_pair_keeps_extra_env() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        cfg.extra_env
            .insert("FLEET_HARNESS".into(), "ollama".into());
        persist_pair(
            &mut cfg,
            "https://agentfleet.studio",
            &pair_resp("https://agentfleet.studio"),
        )
        .unwrap();
        let loaded = LocalConfig::load_or_default().unwrap();
        assert_eq!(
            loaded.extra_env.get("FLEET_HARNESS").map(String::as_str),
            Some("ollama")
        );
    }

    #[test]
    fn refuse_url_switch_without_force() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        crate::config::write_machine_token("flm_existing").unwrap();
        let mut cfg = LocalConfig::defaults();
        cfg.control_plane_url = Some("https://agentfleet.studio".into());
        let err = refuse_url_switch(&cfg, "http://192.168.1.1:18280", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--force"), "{err}");
        refuse_url_switch(&cfg, "http://192.168.1.1:18280", true).unwrap();
    }

    #[test]
    fn persist_pair_accepts_non_loopback_response() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        persist_pair(
            &mut cfg,
            "http://127.0.0.1:9",
            &pair_resp("http://api.example.test:8080"),
        )
        .unwrap();
        assert_eq!(
            cfg.control_plane_url.as_deref(),
            Some("http://api.example.test:8080")
        );
    }

    /// catalog:unit-run-401-reclaims-via-bootstrap — rejected machine.token
    /// reclaims with the same machine.id; it must not wipe the id file.
    #[test]
    fn run_401_reclaims_via_bootstrap_without_wiping_machine_id() {
        let src = include_str!("run.rs");
        let prod = src.split("mod tests {").next().unwrap();
        assert!(
            prod.contains("reclaim_machine_token") && prod.contains("bootstrap_until_workspace"),
            "401 must reclaim via bootstrap, not exit"
        );
        assert!(
            prod.contains("same machine.id"),
            "reclaim comment must name machine.id"
        );
        let reclaim = prod
            .split("async fn reclaim_machine_token")
            .nth(1)
            .expect("reclaim_machine_token");
        assert!(
            !reclaim.contains("delete_machine_token") && !reclaim.contains("unpair("),
            "reclaim must not wipe machine.token/id before bootstrap succeeds"
        );
    }

    #[test]
    fn persist_pair_rejects_empty_machine_token() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        let mut resp = pair_resp("http://api.example.test:8080");
        resp.machine_token.clear();
        let err = persist_pair(&mut cfg, "http://api.example.test:8080", &resp)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("machine_token"),
            "empty token must fail closed: {err}"
        );
        assert!(
            cfg.host_id.is_none(),
            "must not persist a host without a token"
        );
    }

    #[test]
    fn persist_pair_never_stores_unusable_bind_all_url() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        let err = persist_pair(
            &mut cfg,
            "http://0.0.0.0:8080",
            &pair_resp("http://0.0.0.0:8080"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("not reachable") || err.contains("bind"),
            "0.0.0.0 / [::] must fail closed: {err}"
        );
        assert!(
            cfg.control_plane_url
                .as_deref()
                .is_none_or(|s| !s.contains("0.0.0.0") && !s.contains("[::]")),
            "must not persist a bind address: {:?}",
            cfg.control_plane_url
        );
    }

    #[test]
    fn persist_pair_treats_ipv6_loopback_response_as_loopback() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
        }
        let mut cfg = LocalConfig::defaults();
        persist_pair(
            &mut cfg,
            "http://api.agent-fleet.svc.cluster.local:8080",
            &pair_resp("http://[::1]:8080"),
        )
        .unwrap();
        assert_eq!(
            cfg.control_plane_url.as_deref(),
            Some("http://api.agent-fleet.svc.cluster.local:8080"),
            "API advertising [::1] must not replace the locator the host actually reached"
        );
    }
}
