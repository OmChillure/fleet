//! Pull CP-served bins and swap them in place.
//!
//! Never PATCH `paused` / `drain`. Download while still claiming. Host-bin
//! apply sets `software.updating`, stops new leases, waits for in-flight
//! jobs, then swaps (Unix `mv` + exec) or writes `update.cmd` and exits
//! (Windows — a running PE cannot be overwritten).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::client::{DesiredBin, DesiredSoftware, HostClient, HostPublic};
use crate::config::LocalConfig;
use crate::platform::detect_platform;
use crate::software::{PACKAGE_BINS, file_sha256, package_bin_file_name, resolve_bin, sha256_hex};

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_FILE_MACHINE_ARM64: u16 = 0xAA64;

/// Coordinated across the run loop and a background apply task.
#[derive(Debug, Default)]
pub struct UpdateState {
    pub stop_claiming: AtomicBool,
    pub updating: AtomicBool,
    pub exit_process: AtomicBool,
    attempted: Mutex<HashSet<String>>,
    applying: Mutex<bool>,
}

impl UpdateState {
    pub fn new() -> Self {
        Self::default()
    }

    /// One attempt per desired git sha (or bin-hash digest if sha is empty).
    pub fn try_begin(&self, attempt_key: &str) -> bool {
        if attempt_key.is_empty() {
            return false;
        }
        let mut applying = self.applying.lock().unwrap_or_else(|e| e.into_inner());
        if *applying {
            return false;
        }
        let mut seen = self.attempted.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.insert(attempt_key.to_string()) {
            return false;
        }
        *applying = true;
        true
    }

    pub fn finish(&self) {
        if let Ok(mut g) = self.applying.lock() {
            *g = false;
        }
    }

    pub fn is_updating(&self) -> bool {
        self.updating.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyKind {
    Noop,
    ChildrenOnly,
    HostRestart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Noop,
    QuietNoPrebuilt,
    ChildrenSwapped,
    HostRestartPending,
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// Force the Windows PE-lock path (write `update.cmd`, do not open dest).
    pub windows_self_replace: bool,
    /// Do not exec / spawn the updater (unit tests).
    pub dry_restart: bool,
}

impl ApplyOptions {
    pub fn production() -> Self {
        Self {
            windows_self_replace: cfg!(windows),
            dry_restart: false,
        }
    }
}

/// Plane can only restrict; the box can only restrict further. Both default true.
pub fn effective_auto_update(local: bool, cp: bool) -> bool {
    local && cp
}

pub fn should_apply_now(
    local_auto: bool,
    host: &HostPublic,
    desired: Option<&DesiredSoftware>,
    manual: bool,
) -> bool {
    if desired.is_none() {
        return false;
    }
    if manual {
        return true;
    }
    if host
        .update_requested_at
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    effective_auto_update(local_auto, host.auto_update_or_default())
}

pub fn attempt_key(desired: &DesiredSoftware) -> String {
    if !desired.git_sha.trim().is_empty() {
        return desired.git_sha.trim().to_string();
    }
    let mut parts: Vec<String> = desired
        .bins
        .iter()
        .map(|(k, v)| format!("{}:{}", k, v.sha256.to_ascii_lowercase()))
        .collect();
    parts.sort();
    parts.join(",")
}

pub fn lookup_desired<'a>(desired: &'a DesiredSoftware, logical: &str) -> Option<&'a DesiredBin> {
    let exe = format!("{logical}.exe");
    desired.bins.get(&exe).or_else(|| desired.bins.get(logical))
}

#[derive(Debug, Clone)]
pub struct LocalInventory {
    pub platform: String,
    pub windows: bool,
    pub bins: HashMap<String, LocalBin>,
}

#[derive(Debug, Clone)]
pub struct LocalBin {
    pub path: PathBuf,
    pub sha256: Option<String>,
}

pub fn inventory_in(cfg: &LocalConfig, dest_dir: &Path, platform: &str) -> LocalInventory {
    let windows = platform.starts_with("windows-");
    let host_exe = dest_dir.join(package_bin_file_name("fleet-host", windows));
    let host_exe = if host_exe.is_file() {
        Some(host_exe)
    } else {
        std::env::current_exe().ok()
    };
    let mut bins = HashMap::new();
    for logical in PACKAGE_BINS {
        let path = if logical == "fleet-host" {
            host_exe
                .clone()
                .unwrap_or_else(|| dest_dir.join(package_bin_file_name(logical, windows)))
        } else {
            let configured = if logical == "fleet-worker" {
                cfg.fleet_worker_bin.as_str()
            } else {
                cfg.fleet_driver_bin.as_str()
            };
            resolve_bin(configured, host_exe.as_deref())
                .unwrap_or_else(|| dest_dir.join(package_bin_file_name(logical, windows)))
        };
        let sha256 = if path.is_file() {
            file_sha256(&path)
        } else {
            None
        };
        bins.insert(logical.to_string(), LocalBin { path, sha256 });
    }
    LocalInventory {
        platform: platform.to_string(),
        windows,
        bins,
    }
}

pub fn bins_match(local: &LocalInventory, desired: &DesiredSoftware) -> bool {
    if desired.bins.is_empty() {
        return true;
    }
    for logical in PACKAGE_BINS {
        let Some(want) = lookup_desired(desired, logical) else {
            continue;
        };
        let Some(have) = local.bins.get(logical).and_then(|b| b.sha256.as_deref()) else {
            return false;
        };
        if !sha_eq(have, &want.sha256) {
            return false;
        }
    }
    true
}

pub fn plan_kind(local: &LocalInventory, desired: &DesiredSoftware) -> ApplyKind {
    if bins_match(local, desired) {
        return ApplyKind::Noop;
    }
    let host_changed = lookup_desired(desired, "fleet-host").is_some_and(|want| {
        local
            .bins
            .get("fleet-host")
            .and_then(|b| b.sha256.as_deref())
            .is_none_or(|have| !sha_eq(have, &want.sha256))
    });
    if host_changed {
        ApplyKind::HostRestart
    } else {
        ApplyKind::ChildrenOnly
    }
}

fn sha_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn expected_pe_machine(platform: &str) -> Option<u16> {
    match platform {
        "windows-x86_64" => Some(IMAGE_FILE_MACHINE_AMD64),
        "windows-arm64" => Some(IMAGE_FILE_MACHINE_ARM64),
        _ => None,
    }
}

/// PE `Machine` from the COFF header. `None` if the bytes are not a PE.
pub fn pe_machine_type(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 64 || bytes[0] != b'M' || bytes[1] != b'Z' {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().ok()?) as usize;
    if e_lfanew.checked_add(6)? > bytes.len() {
        return None;
    }
    if &bytes[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(
        bytes[e_lfanew + 4..e_lfanew + 6].try_into().ok()?,
    ))
}

pub fn check_pe_machine(bytes: &[u8], platform: &str) -> Result<()> {
    let Some(want) = expected_pe_machine(platform) else {
        return Ok(());
    };
    let got =
        pe_machine_type(bytes).ok_or_else(|| anyhow::anyhow!("downloaded bin is not a PE"))?;
    if got != want {
        bail!("PE machine 0x{got:04X} does not match {platform} (want 0x{want:04X})");
    }
    Ok(())
}

/// `update.cmd` next to the bins. Wait for fleet-host.exe, `move /Y` `.new`, start run.
pub fn write_windows_updater(bin_dir: &Path, changed_files: &[String]) -> Result<PathBuf> {
    let script = bin_dir.join("update.cmd");
    let mut body = String::from(
        "@echo off\r\n\
         setlocal EnableExtensions\r\n\
         set \"DIR=%~dp0\"\r\n\
         :wait\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         tasklist /FI \"IMAGENAME eq fleet-host.exe\" 2>nul | find /I \"fleet-host.exe\" >nul\r\n\
         if not errorlevel 1 goto wait\r\n",
    );
    for name in changed_files {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        body.push_str(&format!(
            "if exist \"%DIR%{name}.new\" move /Y \"%DIR%{name}.new\" \"%DIR%{name}\"\r\n"
        ));
    }
    body.push_str("start \"\" \"%DIR%fleet-host.exe\" run\r\n");
    fs::write(&script, body).with_context(|| format!("write {}", script.display()))?;
    Ok(script)
}

fn dest_file_name(logical: &str, windows: bool) -> String {
    package_bin_file_name(logical, windows)
}

fn staged_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().unwrap_or_default();
    dest.with_file_name(format!("{}.new", name.to_string_lossy()))
}

fn bak_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().unwrap_or_default();
    dest.with_file_name(format!("{}.bak", name.to_string_lossy()))
}

/// Write `{dest}.new` only. Never opens `dest` for write (Windows PE lock).
pub fn stage_new_file(dest: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let newp = staged_path(dest);
    if let Some(parent) = newp.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&newp, bytes).with_context(|| format!("write {}", newp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&newp)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&newp, perms)?;
    }
    Ok(newp)
}

pub fn discard_staged(dest: &Path) {
    let _ = fs::remove_file(staged_path(dest));
}

/// One-generation `.bak`, then `.new` → dest. Does not open dest for in-place write.
pub fn swap_staged(dest: &Path) -> Result<()> {
    let newp = staged_path(dest);
    if !newp.is_file() {
        bail!("missing staged {}", newp.display());
    }
    let bak = bak_path(dest);
    if bak.exists() {
        let _ = fs::remove_file(&bak);
    }
    if dest.exists() {
        fs::rename(dest, &bak)
            .with_context(|| format!("rename {} → {}", dest.display(), bak.display()))?;
    }
    fs::rename(&newp, dest)
        .with_context(|| format!("rename {} → {}", newp.display(), dest.display()))?;
    Ok(())
}

struct Staged {
    logical: &'static str,
    dest: PathBuf,
    file_name: String,
}

async fn download_changed(
    client: &HostClient,
    platform: &str,
    local: &LocalInventory,
    desired: &DesiredSoftware,
) -> Result<HashMap<&'static str, Vec<u8>>> {
    let mut out = HashMap::new();
    for logical in PACKAGE_BINS {
        let Some(want) = lookup_desired(desired, logical) else {
            continue;
        };
        let have = local
            .bins
            .get(logical)
            .and_then(|b| b.sha256.as_deref())
            .unwrap_or("");
        if sha_eq(have, &want.sha256) {
            continue;
        }
        let file_name = dest_file_name(logical, local.windows);
        let sidecar = match client.fetch_install_sha256(platform, &file_name).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no prebuilt") {
                    bail!("no prebuilt {file_name} for {platform}");
                }
                return Err(e);
            }
        };
        if !sha_eq(&sidecar, &want.sha256) {
            bail!(
                "manifest sha256 for {file_name} does not match sidecar ({sidecar} vs {})",
                want.sha256
            );
        }
        let bytes = client.fetch_install_bytes(platform, &file_name).await?;
        let got = sha256_hex(&bytes);
        if !sha_eq(&got, &want.sha256) {
            bail!("checksum mismatch for {file_name}");
        }
        if let Some(sz) = want.size
            && sz > 0
            && bytes.len() as u64 != sz
        {
            bail!(
                "size mismatch for {file_name}: got {} want {sz}",
                bytes.len()
            );
        }
        check_pe_machine(&bytes, platform)?;
        info!(bin = %file_name, bytes = bytes.len(), "downloaded update");
        out.insert(logical, bytes);
    }
    Ok(out)
}

fn stage_all(
    local: &LocalInventory,
    downloaded: &HashMap<&'static str, Vec<u8>>,
) -> Result<Vec<Staged>> {
    let mut staged: Vec<Staged> = Vec::new();
    for logical in PACKAGE_BINS {
        let Some(bytes) = downloaded.get(logical) else {
            continue;
        };
        let dest = local
            .bins
            .get(logical)
            .map(|b| b.path.clone())
            .ok_or_else(|| anyhow::anyhow!("missing dest for {logical}"))?;
        match stage_new_file(&dest, bytes) {
            Ok(newp) => {
                let on_disk = file_sha256(&newp);
                let expect = sha256_hex(bytes);
                if on_disk.as_deref().is_none_or(|h| !sha_eq(h, &expect)) {
                    discard_staged(&dest);
                    for s in &staged {
                        discard_staged(&s.dest);
                    }
                    bail!("staged checksum failed for {logical}");
                }
                staged.push(Staged {
                    logical,
                    file_name: dest
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    dest,
                });
            }
            Err(e) => {
                for s in &staged {
                    discard_staged(&s.dest);
                }
                return Err(e);
            }
        }
    }
    Ok(staged)
}

fn discard_all(staged: &[Staged]) {
    for s in staged {
        discard_staged(&s.dest);
    }
}

fn swap_named(staged: &[Staged], only: impl Fn(&Staged) -> bool) -> Result<()> {
    for s in staged.iter().filter(|s| only(s)) {
        swap_staged(&s.dest)?;
    }
    Ok(())
}

async fn wait_jobs_idle(jobs: &AsyncMutex<HashSet<Uuid>>) {
    loop {
        if jobs.lock().await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn unix_restart(exe: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(exe).arg("run").exec();
        Err(anyhow::anyhow!("exec {}: {err}", exe.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = exe;
        bail!("unix restart on non-unix");
    }
}

fn spawn_windows_updater(script: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        std::process::Command::new("cmd.exe")
            .args(["/C", &script.to_string_lossy()])
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .with_context(|| format!("spawn {}", script.display()))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = script;
        Ok(())
    }
}

/// Shared apply path for `fleet-host update` and auto-apply after heartbeat.
///
/// Does not PATCH the control plane. `jobs` is the in-process claim set
/// (empty for the CLI).
pub async fn apply_desired(
    client: &HostClient,
    cfg: &LocalConfig,
    desired: &DesiredSoftware,
    dest_dir: &Path,
    jobs: &AsyncMutex<HashSet<Uuid>>,
    state: &UpdateState,
    opts: &ApplyOptions,
) -> Result<ApplyOutcome> {
    let platform = detect_platform();
    if !desired.platform.is_empty() && desired.platform != platform {
        warn!(
            desired = %desired.platform,
            local = %platform,
            "desired platform does not match this box; skipping apply"
        );
        return Ok(ApplyOutcome::Noop);
    }
    if desired.bins.is_empty() {
        return Ok(ApplyOutcome::Noop);
    }

    let local = inventory_in(cfg, dest_dir, &platform);
    let kind = plan_kind(&local, desired);
    if kind == ApplyKind::Noop {
        info!("software already matches desired; no download");
        return Ok(ApplyOutcome::Noop);
    }

    // Tell the plane immediately so the Workers card can say "updating".
    state.updating.store(true, Ordering::SeqCst);

    let downloaded = match download_changed(client, &platform, &local, desired).await {
        Ok(d) => d,
        Err(e) => {
            state.updating.store(false, Ordering::SeqCst);
            let msg = e.to_string();
            if msg.contains("no prebuilt") {
                info!(error = %msg, "no prebuilt; staying on current bins");
                return Ok(ApplyOutcome::QuietNoPrebuilt);
            }
            return Err(e);
        }
    };
    if downloaded.is_empty() {
        state.updating.store(false, Ordering::SeqCst);
        return Ok(ApplyOutcome::Noop);
    }

    let staged = match stage_all(&local, &downloaded) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "update stage failed; old bins kept");
            return Err(e);
        }
    };

    match kind {
        ApplyKind::Noop => {
            discard_all(&staged);
            Ok(ApplyOutcome::Noop)
        }
        ApplyKind::ChildrenOnly => {
            if let Err(e) = swap_named(&staged, |s| s.logical != "fleet-host") {
                discard_all(&staged);
                return Err(e);
            }
            info!("child bins swapped; still claiming");
            state.updating.store(false, Ordering::SeqCst);
            Ok(ApplyOutcome::ChildrenSwapped)
        }
        ApplyKind::HostRestart => {
            state.updating.store(true, Ordering::SeqCst);
            state.stop_claiming.store(true, Ordering::SeqCst);
            wait_jobs_idle(jobs).await;
            if opts.windows_self_replace {
                let names: Vec<String> = staged.iter().map(|s| s.file_name.clone()).collect();
                match write_windows_updater(dest_dir, &names) {
                    Ok(script) => {
                        info!(path = %script.display(), "wrote Windows updater; exiting so PE can be replaced");
                        if !opts.dry_restart {
                            spawn_windows_updater(&script)?;
                            state.exit_process.store(true, Ordering::SeqCst);
                        }
                        Ok(ApplyOutcome::HostRestartPending)
                    }
                    Err(e) => {
                        discard_all(&staged);
                        state.updating.store(false, Ordering::SeqCst);
                        state.stop_claiming.store(false, Ordering::SeqCst);
                        Err(e)
                    }
                }
            } else {
                if let Err(e) = swap_named(&staged, |_| true) {
                    discard_all(&staged);
                    state.updating.store(false, Ordering::SeqCst);
                    state.stop_claiming.store(false, Ordering::SeqCst);
                    return Err(e);
                }
                info!("host bin swapped; restarting");
                if opts.dry_restart {
                    return Ok(ApplyOutcome::HostRestartPending);
                }
                let exe = local
                    .bins
                    .get("fleet-host")
                    .map(|b| b.path.clone())
                    .unwrap_or_else(|| dest_dir.join("fleet-host"));
                unix_restart(&exe)?;
                Ok(ApplyOutcome::HostRestartPending)
            }
        }
    }
}

/// Decide whether the run loop should start a background apply.
pub fn maybe_queue_auto(
    state: &UpdateState,
    cfg: &LocalConfig,
    host: &HostPublic,
    desired: Option<&DesiredSoftware>,
) -> Option<DesiredSoftware> {
    let desired = desired.cloned()?;
    if !should_apply_now(cfg.auto_update, host, Some(&desired), false) {
        return None;
    }
    let mut key = attempt_key(&desired);
    if let Some(t) = host
        .update_requested_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        key.push('|');
        key.push_str(t);
    }
    if !state.try_begin(&key) {
        return None;
    }
    Some(desired)
}

/// Manual `fleet-host update` — same apply path, ignores local/CP auto flags.
pub async fn run_manual() -> Result<()> {
    let cfg = LocalConfig::load_or_default()?;
    let token = crate::config::read_machine_token()?.ok_or_else(|| {
        anyhow::anyhow!(
            "not paired: missing {}",
            crate::paths::token_path().display()
        )
    })?;
    let url = cfg.control_plane_url.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "config missing control_plane_url ({})",
            crate::paths::config_path().display()
        )
    })?;
    let client = HostClient::with_machine_token(&url, token, &cfg)?;
    let resp = client.heartbeat(&cfg, 0, None, false).await?;
    let Some(desired) = resp.desired else {
        println!("no prebuilt for {}", detect_platform());
        return Ok(());
    };
    let dest = install_dir()?;
    let jobs = AsyncMutex::new(HashSet::new());
    let state = UpdateState::new();
    let key = attempt_key(&desired);
    let _ = state.try_begin(&key);
    let outcome = apply_desired(
        &client,
        &cfg,
        &desired,
        &dest,
        &jobs,
        &state,
        &ApplyOptions::production(),
    )
    .await;
    state.finish();
    match outcome {
        Ok(ApplyOutcome::Noop) => {
            println!("already current ({})", desired.git_sha);
            Ok(())
        }
        Ok(ApplyOutcome::QuietNoPrebuilt) => {
            println!("no prebuilt for {}", detect_platform());
            Ok(())
        }
        Ok(ApplyOutcome::ChildrenSwapped) => {
            println!("updated worker/driver bins");
            Ok(())
        }
        Ok(ApplyOutcome::HostRestartPending) => {
            if state.exit_process.load(Ordering::SeqCst) {
                std::process::exit(0);
            }
            println!("host binary updated; restart fleet-host run");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent"))
}

/// Used only by tests that inject bytes without HTTP.
#[cfg(test)]
pub fn verify_bytes(
    name: &str,
    bytes: &[u8],
    want_sha: &str,
    sidecar: Option<&[u8]>,
    platform: &str,
) -> Result<()> {
    use crate::client::parse_sha256_sidecar;
    if let Some(side) = sidecar {
        let parsed = parse_sha256_sidecar(side).ok_or_else(|| anyhow::anyhow!("bad sidecar"))?;
        if !sha_eq(&parsed, want_sha) {
            bail!("sidecar mismatch for {name}");
        }
    } else {
        bail!("missing sha256 sidecar for {name}");
    }
    let got = sha256_hex(bytes);
    if !sha_eq(&got, want_sha) {
        bail!("checksum mismatch for {name}");
    }
    check_pe_machine(bytes, platform)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DesiredBin;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::thread;

    fn sha(s: &[u8]) -> String {
        sha256_hex(s)
    }

    fn desired_for(platform: &str, bins: &[(&str, &[u8])]) -> DesiredSoftware {
        let mut map = HashMap::new();
        for (name, bytes) in bins {
            map.insert(
                (*name).to_string(),
                DesiredBin {
                    sha256: sha(bytes),
                    size: Some(bytes.len() as u64),
                },
            );
        }
        DesiredSoftware {
            git_sha: "deadbeef".into(),
            platform: platform.into(),
            bins: map,
        }
    }

    fn write_bin(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&p, perms).unwrap();
        }
        p
    }

    fn local_from_dir(dir: &Path, platform: &str) -> LocalInventory {
        inventory_in(&LocalConfig::default(), dir, platform)
    }

    #[test]
    fn hash_match_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let host = b"host-v1";
        let worker = b"worker-v1";
        let driver = b"driver-v1";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-worker", worker);
        write_bin(dir.path(), "fleet-driver", driver);
        let desired = desired_for(
            "linux-x86_64",
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-worker", worker.as_slice()),
                ("fleet-driver", driver.as_slice()),
            ],
        );
        let local = local_from_dir(dir.path(), "linux-x86_64");
        assert!(bins_match(&local, &desired));
        assert_eq!(plan_kind(&local, &desired), ApplyKind::Noop);
    }

    #[test]
    fn checksum_fail_leaves_old_bins() {
        let dir = tempfile::tempdir().unwrap();
        let old = b"old-worker";
        write_bin(dir.path(), "fleet-host", b"host");
        write_bin(dir.path(), "fleet-worker", old);
        write_bin(dir.path(), "fleet-driver", b"driver");
        let dest = dir.path().join("fleet-worker");
        let want = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let bad = b"not-the-bytes-you-wanted";
        let err = verify_bytes(
            "fleet-worker",
            bad,
            want,
            Some(format!("{want}  fleet-worker\n").as_bytes()),
            "linux-x86_64",
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        // Stage then detect failure and discard — dest stays old.
        let staged = stage_new_file(&dest, bad).unwrap();
        assert!(staged.is_file());
        discard_staged(&dest);
        assert!(!staged.is_file());
        assert_eq!(fs::read(&dest).unwrap(), old);
    }

    #[test]
    fn child_only_apply_does_not_require_jobs_idle() {
        let dir = tempfile::tempdir().unwrap();
        write_bin(dir.path(), "fleet-host", b"host-same");
        write_bin(dir.path(), "fleet-worker", b"worker-old");
        write_bin(dir.path(), "fleet-driver", b"driver-old");
        let new_w = b"worker-new";
        let new_d = b"driver-new";
        let desired = desired_for(
            "linux-x86_64",
            &[
                ("fleet-host", b"host-same".as_slice()),
                ("fleet-worker", new_w.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        let local = local_from_dir(dir.path(), "linux-x86_64");
        assert_eq!(plan_kind(&local, &desired), ApplyKind::ChildrenOnly);
        let dest_w = dir.path().join("fleet-worker");
        let dest_d = dir.path().join("fleet-driver");
        stage_new_file(&dest_w, new_w).unwrap();
        stage_new_file(&dest_d, new_d).unwrap();
        // jobs != 0 is fine
        swap_staged(&dest_w).unwrap();
        swap_staged(&dest_d).unwrap();
        assert_eq!(fs::read(&dest_w).unwrap(), new_w);
        assert_eq!(fs::read(&dest_d).unwrap(), new_d);
        assert_eq!(
            fs::read(dir.path().join("fleet-host")).unwrap(),
            b"host-same"
        );
    }

    #[test]
    fn local_auto_update_false_does_not_apply() {
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = DesiredSoftware {
            git_sha: "abc".into(),
            platform: "linux-x86_64".into(),
            bins: HashMap::from([(
                "fleet-host".into(),
                DesiredBin {
                    sha256: "aa".into(),
                    size: None,
                },
            )]),
        };
        assert!(!effective_auto_update(false, true));
        assert!(!should_apply_now(false, &host, Some(&desired), false));
        assert!(should_apply_now(false, &host, Some(&desired), true));
        assert!(should_apply_now(true, &host, Some(&desired), false));
        let off_cp = HostPublic {
            auto_update: Some(false),
            ..HostPublic::default()
        };
        assert!(!should_apply_now(true, &off_cp, Some(&desired), false));
        let requested = HostPublic {
            auto_update: Some(false),
            update_requested_at: Some("2026-01-01T00:00:00Z".into()),
            ..HostPublic::default()
        };
        assert!(should_apply_now(false, &requested, Some(&desired), false));
    }

    #[test]
    fn windows_updater_script_written_running_exe_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_bin(dir.path(), "fleet-host.exe", b"running-pe");
        let worker = write_bin(dir.path(), "fleet-worker.exe", b"old-w");
        // Hold dest open (simulates a locked PE) — apply must not truncate it.
        let mut held = fs::OpenOptions::new().read(true).open(&exe).unwrap();
        let mut probe = [0u8; 4];
        held.read_exact(&mut probe).unwrap();
        assert_eq!(&probe, b"runn");

        let new_host = b"new-host-bytes";
        let staged = stage_new_file(&exe, new_host).unwrap();
        assert!(staged.ends_with("fleet-host.exe.new"));
        assert_eq!(fs::read(&exe).unwrap(), b"running-pe");
        assert_eq!(fs::read(&staged).unwrap(), new_host);

        let script = write_windows_updater(
            dir.path(),
            &["fleet-host.exe".into(), "fleet-worker.exe".into()],
        )
        .unwrap();
        assert_eq!(script.file_name().unwrap(), "update.cmd");
        let text = fs::read_to_string(&script).unwrap();
        assert!(text.contains("move /Y"), "{text}");
        assert!(text.contains("fleet-host.exe.new"), "{text}");
        assert!(
            text.contains("start \"\" \"%DIR%fleet-host.exe\" run"),
            "{text}"
        );
        assert!(!text.to_ascii_lowercase().contains("paused"));
        assert!(!text.to_ascii_lowercase().contains("drain"));

        // dest still the original bytes; we never opened it for write.
        assert_eq!(fs::read(&exe).unwrap(), b"running-pe");
        assert_eq!(fs::read(&worker).unwrap(), b"old-w");
        drop(held);
    }

    #[test]
    fn pe_machine_type_amd64_and_arm64() {
        fn pe(machine: u16) -> Vec<u8> {
            let mut b = vec![0u8; 0x48];
            b[0] = b'M';
            b[1] = b'Z';
            b[0x3C] = 0x40;
            b[0x40] = b'P';
            b[0x41] = b'E';
            b[0x44] = machine as u8;
            b[0x45] = (machine >> 8) as u8;
            b
        }
        assert_eq!(pe_machine_type(&pe(IMAGE_FILE_MACHINE_AMD64)), Some(0x8664));
        assert_eq!(pe_machine_type(&pe(IMAGE_FILE_MACHINE_ARM64)), Some(0xAA64));
        check_pe_machine(&pe(IMAGE_FILE_MACHINE_AMD64), "windows-x86_64").unwrap();
        assert!(check_pe_machine(&pe(IMAGE_FILE_MACHINE_AMD64), "windows-arm64").is_err());
        check_pe_machine(b"elf-not-pe", "linux-x86_64").unwrap();
    }

    #[test]
    fn one_attempt_per_desired_sha() {
        let state = UpdateState::new();
        assert!(state.try_begin("abc"));
        assert!(!state.try_begin("abc"));
        state.finish();
        assert!(!state.try_begin("abc"));
        assert!(state.try_begin("def"));
    }

    struct MockCp {
        base: String,
        log: Arc<StdMutex<Vec<(String, String)>>>,
        _join: thread::JoinHandle<()>,
    }

    fn spawn_mock(files: HashMap<String, Vec<u8>>) -> MockCp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(StdMutex::new(Vec::new()));
        let log2 = log.clone();
        let files = Arc::new(files);
        let join = thread::spawn(move || {
            listener.set_nonblocking(false).ok();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = vec![0u8; 16384];
                let n = match s.read(&mut buf) {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let first = req.lines().next().unwrap_or("");
                let mut it = first.split_whitespace();
                let method = it.next().unwrap_or("").to_string();
                let path = it.next().unwrap_or("").to_string();
                log2.lock().unwrap().push((method, path.clone()));
                let body = files.get(&path).cloned();
                let (status, payload) = match body {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", Vec::new()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = s.write_all(header.as_bytes());
                let _ = s.write_all(&payload);
                let _ = s.flush();
            }
        });
        MockCp {
            base: format!("http://{addr}"),
            log,
            _join: join,
        }
    }

    fn files_for(platform: &str, bins: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
        let mut m = HashMap::new();
        for (name, bytes) in bins {
            let p = format!("/install/worker/bin/{platform}/{name}");
            m.insert(p.clone(), bytes.to_vec());
            m.insert(
                format!("{p}.sha256"),
                format!("{}\n", sha(bytes)).into_bytes(),
            );
        }
        m
    }

    #[tokio::test]
    async fn apply_does_not_patch_drain_or_paused() {
        let dir = tempfile::tempdir().unwrap();
        let host = b"host-same";
        let old_w = b"worker-old";
        let new_w = b"worker-new";
        let drv = b"driver-same";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-worker", old_w);
        write_bin(dir.path(), "fleet-driver", drv);
        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-worker", new_w.as_slice()),
                ("fleet-driver", drv.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-worker", new_w.as_slice()),
                ("fleet-driver", drv.as_slice()),
            ],
        );
        let jobs = AsyncMutex::new(HashSet::from([Uuid::now_v7()]));
        let state = UpdateState::new();
        let opts = ApplyOptions {
            windows_self_replace: false,
            dry_restart: true,
        };
        let out = apply_desired(&client, &cfg, &desired, dir.path(), &jobs, &state, &opts)
            .await
            .expect("apply");
        assert_eq!(out, ApplyOutcome::ChildrenSwapped);
        assert_eq!(fs::read(dir.path().join("fleet-worker")).unwrap(), new_w);
        assert!(!state.stop_claiming.load(Ordering::SeqCst));
        assert!(!state.is_updating());
        let log = mock.log.lock().unwrap().clone();
        assert!(!log.is_empty(), "expected GET install bytes, got {log:?}");
        for (method, path) in &log {
            assert_eq!(method, "GET", "apply must not {method} {path}");
            assert!(!path.contains("paused"), "{path}");
            assert!(!path.contains("drain"), "{path}");
            assert!(!path.contains("/api/worker-hosts/"), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn host_apply_sets_updating_and_does_not_http_patch() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let w = b"worker-same";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-worker", w);
        write_bin(dir.path(), "fleet-driver", d);
        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-worker", w.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-worker", w.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let jobs = AsyncMutex::new(HashSet::new());
        let state = UpdateState::new();
        let opts = ApplyOptions {
            windows_self_replace: true,
            dry_restart: true,
        };
        let out = apply_desired(&client, &cfg, &desired, dir.path(), &jobs, &state, &opts)
            .await
            .expect("apply");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert!(state.is_updating());
        assert!(state.stop_claiming.load(Ordering::SeqCst));
        assert_eq!(fs::read(dir.path().join("fleet-host")).unwrap(), old_h);
        assert!(
            dir.path().join("fleet-host.new").is_file() || dir.path().join("update.cmd").is_file()
        );
        assert!(dir.path().join("update.cmd").is_file());
        for (method, path) in mock.log.lock().unwrap().iter() {
            assert_eq!(method, "GET", "host apply must not {method} {path}");
            assert!(!path.contains("drain") && !path.contains("paused"));
        }
    }

    #[tokio::test]
    async fn maybe_queue_auto_respects_local_flag() {
        let state = UpdateState::new();
        let cfg_off = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = DesiredSoftware {
            git_sha: "sha1".into(),
            platform: "linux-x86_64".into(),
            bins: HashMap::from([(
                "fleet-host".into(),
                DesiredBin {
                    sha256: "aa".into(),
                    size: None,
                },
            )]),
        };
        assert!(maybe_queue_auto(&state, &cfg_off, &host, Some(&desired)).is_none());
        let cfg_on = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        assert!(maybe_queue_auto(&state, &cfg_on, &host, Some(&desired)).is_some());
        assert!(maybe_queue_auto(&state, &cfg_on, &host, Some(&desired)).is_none());
    }
}
