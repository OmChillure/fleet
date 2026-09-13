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
    if request_stamp(host).is_some() {
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
            let configured = cfg.fleet_driver_bin.as_str();
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

/// Fallback helper. Prefer `swap_staged` while still running (Windows can
/// rename a locked PE; it cannot overwrite it). This script is only used
/// when that rename fails.
///
/// Do not wait forever on `tasklist` — any other `fleet-host.exe` would
/// stall the swap. Cap the wait, log, start with an explicit working dir.
pub fn write_windows_updater(bin_dir: &Path, changed_files: &[String]) -> Result<PathBuf> {
    let script = bin_dir.join("update.cmd");
    let mut body = String::from(
        "@echo off\r\n\
         setlocal EnableExtensions\r\n\
         set \"DIR=%~dp0\"\r\n\
         set \"LOG=%DIR%update.log\"\r\n\
         echo %DATE% %TIME% updater start>>\"%LOG%\"\r\n\
         set /a n=0\r\n\
         :wait\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         set /a n+=1\r\n\
         tasklist /FI \"IMAGENAME eq fleet-host.exe\" 2>nul | find /I \"fleet-host.exe\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           if %n% LSS 60 goto wait\r\n\
           echo %DATE% %TIME% still running after 60s; swapping anyway>>\"%LOG%\"\r\n\
         )\r\n",
    );
    for name in changed_files {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        body.push_str(&format!(
            "if exist \"%DIR%{name}.new\" (\r\n\
             if exist \"%DIR%{name}\" move /Y \"%DIR%{name}\" \"%DIR%{name}.bak\" >>\"%LOG%\" 2>&1\r\n\
             move /Y \"%DIR%{name}.new\" \"%DIR%{name}\" >>\"%LOG%\" 2>&1\r\n\
             )\r\n"
        ));
    }
    body.push_str(
        "echo %DATE% %TIME% starting fleet-host>>\"%LOG%\"\r\n\
         start \"\" /D \"%DIR%\" \"%DIR%fleet-host.exe\" run\r\n",
    );
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

/// Windows can rename a running PE; it cannot overwrite it. Put the new
/// file at the original name, start it detached, then exit. Fall back to
/// `update.cmd` only if rename-aside fails (AV, missing ACL, …).
fn windows_host_restart(
    dest_dir: &Path,
    staged: &[Staged],
    state: &UpdateState,
    opts: &ApplyOptions,
) -> Result<ApplyOutcome> {
    match swap_named(staged, |_| true) {
        Ok(()) => {
            info!("Windows PE renamed aside; new bins are in place");
            if !opts.dry_restart {
                spawn_windows_host(&dest_dir.join("fleet-host.exe"))?;
                state.exit_process.store(true, Ordering::SeqCst);
            }
            Ok(ApplyOutcome::HostRestartPending)
        }
        Err(e) => {
            warn!(error = %e, "rename-aside failed; falling back to update.cmd");
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
                Err(e2) => {
                    discard_all(staged);
                    state.updating.store(false, Ordering::SeqCst);
                    state.stop_claiming.store(false, Ordering::SeqCst);
                    Err(e2)
                }
            }
        }
    }
}

fn spawn_windows_host(exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let dir = exe.parent().unwrap_or(Path::new("."));
        std::process::Command::new(exe)
            .arg("run")
            .current_dir(dir)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| format!("spawn {}", exe.display()))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        Ok(())
    }
}

fn spawn_windows_updater(script: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd.exe")
            .args(["/C", &script.to_string_lossy()])
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
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
                windows_host_restart(dest_dir, &staged, state, opts)
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

fn request_stamp(host: &HostPublic) -> Option<&str> {
    host.update_requested_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
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
    // Host-bin swap / restart pending: do not start another download.
    if state.is_updating() {
        return None;
    }
    let request = request_stamp(host);
    // Auto-off Update now is one-shot: leftover timestamp must not pull a
    // newer sha. Auto-on still keys by desired bits (plus stamp if present).
    let key = if effective_auto_update(cfg.auto_update, host.auto_update_or_default()) {
        let mut key = attempt_key(&desired);
        if let Some(t) = request {
            key.push('|');
            key.push_str(t);
        }
        key
    } else {
        request?.to_string()
    };
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
    let resp = client.heartbeat(&cfg, 0, None, false, None).await?;
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
                    version: None,
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
        let driver = b"driver-v1";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-driver", driver);
        let desired = desired_for(
            "linux-x86_64",
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", driver.as_slice()),
            ],
        );
        let local = local_from_dir(dir.path(), "linux-x86_64");
        assert!(bins_match(&local, &desired));
        assert_eq!(plan_kind(&local, &desired), ApplyKind::Noop);
    }

    /// catalog:unit-request-update — CP auto_update false must restrict a
    /// box that still has local auto on (plane can only restrict).
    #[test]
    fn maybe_queue_auto_local_on_cp_off_without_request_is_none() {
        let state = UpdateState::new();
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let desired = stub_desired("abc");
        assert!(
            maybe_queue_auto(&state, &cfg, &host_auto_off(None), Some(&desired)).is_none(),
            "local auto on + CP auto off + no Update now must not apply"
        );
    }

    /// catalog:unit-request-update — CP restrict must not block Update now.
    #[test]
    fn maybe_queue_auto_local_on_cp_off_update_now_still_applies() {
        let state = UpdateState::new();
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let desired = stub_desired("abc");
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:00Z")),
                Some(&desired)
            )
            .is_some(),
            "Update now applies even when CP auto_update is false"
        );
    }

    /// catalog:unit-request-update — missing host.auto_update (old CP) defaults true.
    #[test]
    fn maybe_queue_auto_omitted_cp_auto_defaults_true() {
        let state = UpdateState::new();
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: None,
            ..HostPublic::default()
        };
        let desired = stub_desired("abc");
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&desired)).is_some(),
            "omitted CP auto_update defaults true and applies when local auto is on"
        );
    }

    fn stub_desired(git_sha: &str) -> DesiredSoftware {
        DesiredSoftware {
            git_sha: git_sha.into(),
            platform: "linux-x86_64".into(),
            bins: HashMap::from([(
                "fleet-host".into(),
                DesiredBin {
                    sha256: "aa".into(),
                    size: None,
                    version: None,
                },
            )]),
        }
    }

    fn host_auto_off(update_requested_at: Option<&str>) -> HostPublic {
        HostPublic {
            auto_update: Some(false),
            update_requested_at: update_requested_at.map(str::to_string),
            ..HostPublic::default()
        }
    }

    /// catalog:unit-request-update — Auto-update off + Update now is one-shot.
    /// After one apply the request is consumed (leftover timestamp must not
    /// pull again if desired bits change). Mid-apply must not start another.
    #[test]
    fn update_now_is_one_shot_when_auto_off() {
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let desired_a = stub_desired("sha-a");
        let desired_b = stub_desired("sha-b");
        let state = UpdateState::new();

        assert!(
            maybe_queue_auto(&state, &cfg, &host_auto_off(Some("   ")), Some(&desired_a)).is_none(),
            "whitespace update_requested_at is not a request"
        );

        let req = host_auto_off(Some("2026-01-01T00:00:00Z"));
        assert!(
            maybe_queue_auto(&state, &cfg, &req, Some(&desired_a)).is_some(),
            "Update now applies once even when local and CP auto are off"
        );
        let later = host_auto_off(Some("2026-01-01T00:00:01Z"));
        assert!(
            maybe_queue_auto(&state, &cfg, &later, Some(&desired_b)).is_none(),
            "mid-apply must not pull even if the operator clicks Update now again"
        );

        state.finish();
        assert!(
            maybe_queue_auto(&state, &cfg, &req, Some(&desired_b)).is_none(),
            "leftover update_requested_at + new desired must not pull when auto is off"
        );
    }

    /// catalog:unit-request-update — auto-on keys by sha|stamp so a leftover
    /// Update now still restages a newer desired sha.
    #[test]
    fn auto_on_leftover_stamp_plus_new_desired_applies() {
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            update_requested_at: Some("2026-01-01T00:00:00Z".into()),
            ..HostPublic::default()
        };
        let state = UpdateState::new();
        assert!(maybe_queue_auto(&state, &cfg, &host, Some(&stub_desired("sha-a"))).is_some());
        state.finish();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&stub_desired("sha-b"))).is_some(),
            "auto-on leftover update_requested_at + new git_sha must apply"
        );
    }

    /// catalog:unit-request-update — request_stamp trims; a padded copy of the
    /// consumed stamp is the same one-shot key.
    #[test]
    fn padded_update_requested_at_trims_to_same_one_shot_key() {
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let desired = stub_desired("sha-a");
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:00Z")),
                Some(&desired)
            )
            .is_some()
        );
        state.finish();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("  2026-01-01T00:00:00Z  ")),
                Some(&desired)
            )
            .is_none(),
            "padded copy of the consumed stamp must not re-queue"
        );
    }

    /// catalog:unit-request-update — empty update_requested_at is not a request.
    #[test]
    fn empty_string_update_requested_at_is_not_a_request() {
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let desired = stub_desired("sha-a");
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &host_auto_off(Some("")), Some(&desired)).is_none(),
            "empty update_requested_at is not a request"
        );
    }

    #[test]
    fn windows_updater_script_written_running_exe_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_windows_updater(dir.path(), &["fleet-host.exe".into()]).unwrap();
        assert_eq!(script.file_name().unwrap(), "update.cmd");
        let text = fs::read_to_string(&script).unwrap();
        assert!(text.contains("move /Y"), "{text}");
        assert!(text.contains("fleet-host.exe.new"), "{text}");
        assert!(text.contains("LSS 60"), "must not wait forever: {text}");
        assert!(
            text.contains("start \"\" /D \"%DIR%\" \"%DIR%fleet-host.exe\" run"),
            "{text}"
        );
    }

    #[test]
    fn windows_host_restart_writes_update_cmd_when_rename_aside_fails() {
        let dir = tempfile::tempdir().unwrap();
        let dest = write_bin(dir.path(), "fleet-host.exe", b"running-pe");
        stage_new_file(&dest, b"new-host-bytes").unwrap();
        // Non-empty dest.bak makes swap_staged's dest→bak rename fail.
        let bak = dir.path().join("fleet-host.exe.bak");
        fs::create_dir(&bak).unwrap();
        fs::write(bak.join("keep"), b"x").unwrap();
        let staged = [Staged {
            logical: "fleet-host",
            dest: dest.clone(),
            file_name: "fleet-host.exe".into(),
        }];
        let state = UpdateState::new();
        let opts = ApplyOptions {
            windows_self_replace: true,
            dry_restart: true,
        };
        let out = windows_host_restart(dir.path(), &staged, &state, &opts).unwrap();
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert!(dir.path().join("update.cmd").is_file());
        assert_eq!(fs::read(&dest).unwrap(), b"running-pe");
    }

    #[test]
    fn write_windows_updater_moves_every_changed_bin() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_windows_updater(
            dir.path(),
            &["fleet-host.exe".into(), "fleet-driver.exe".into()],
        )
        .unwrap();
        let text = fs::read_to_string(&script).unwrap();
        assert!(text.contains("fleet-host.exe.new"), "{text}");
        assert!(text.contains("fleet-driver.exe.new"), "{text}");
        assert!(
            text.contains("move /Y \"%DIR%fleet-host.exe\" \"%DIR%fleet-host.exe.bak\""),
            "{text}"
        );
        assert!(
            text.contains("move /Y \"%DIR%fleet-driver.exe\" \"%DIR%fleet-driver.exe.bak\""),
            "{text}"
        );
        assert!(
            text.contains("move /Y \"%DIR%fleet-host.exe.new\" \"%DIR%fleet-host.exe\""),
            "{text}"
        );
        assert!(
            text.contains("move /Y \"%DIR%fleet-driver.exe.new\" \"%DIR%fleet-driver.exe\""),
            "{text}"
        );
    }

    #[test]
    fn write_windows_updater_skips_blank_changed_names() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_windows_updater(
            dir.path(),
            &["".into(), "  ".into(), "fleet-host.exe".into()],
        )
        .unwrap();
        let text = fs::read_to_string(&script).unwrap();
        assert!(
            !text.contains("move /Y \"%DIR%\" \"%DIR%.bak\""),
            "{text}"
        );
        assert!(!text.contains("%DIR%  .bak"), "{text}");
        assert!(text.contains("fleet-host.exe.new"), "{text}");
    }

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

    #[test]
    fn pe_machine_type_amd64_and_arm64() {
        let amd64 = pe(IMAGE_FILE_MACHINE_AMD64);
        let arm64 = pe(IMAGE_FILE_MACHINE_ARM64);
        assert_eq!(pe_machine_type(&amd64), Some(0x8664));
        assert_eq!(pe_machine_type(&arm64), Some(0xAA64));
        check_pe_machine(&amd64, "windows-x86_64")
            .expect("windows-x86_64 must require AMD64 PE, not skip");
        let err = check_pe_machine(&arm64, "windows-x86_64")
            .expect_err("windows-x86_64 must reject ARM64 (mapping is not skip)");
        assert!(
            err.to_string().contains("does not match windows-x86_64"),
            "{err}"
        );
    }

    #[test]
    fn check_pe_machine_rejects_non_pe_on_windows() {
        let err = check_pe_machine(b"elf-not-pe", "windows-x86_64")
            .expect_err("windows-x86_64 must reject non-PE");
        assert!(err.to_string().contains("not a PE"), "{err}");
        let err = check_pe_machine(b"MZ", "windows-x86_64")
            .expect_err("short not-PE bytes must reject on windows-x86_64");
        assert!(err.to_string().contains("not a PE"), "{err}");
        check_pe_machine(b"elf-not-pe", "linux-x86_64")
            .expect("linux-x86_64 must skip the PE gate");
    }

    #[test]
    fn check_pe_machine_accepts_arm64_on_windows_arm64() {
        check_pe_machine(&pe(IMAGE_FILE_MACHINE_ARM64), "windows-arm64")
            .expect("windows-arm64 must accept ARM64 PE");
        let err = check_pe_machine(&pe(IMAGE_FILE_MACHINE_AMD64), "windows-arm64")
            .expect_err("windows-arm64 must reject AMD64 PE");
        assert!(
            err.to_string().contains("does not match windows-arm64"),
            "{err}"
        );
    }

    #[test]
    fn pe_machine_type_none_when_truncated_or_not_mz() {
        assert_eq!(pe_machine_type(&[0u8; 63]), None);
        assert_eq!(pe_machine_type(b"MZ"), None);
        let mut not_mz = vec![0u8; 64];
        not_mz[0] = b'E';
        not_mz[1] = b'L';
        assert_eq!(pe_machine_type(&not_mz), None);

        let mut past_end = vec![0u8; 64];
        past_end[0] = b'M';
        past_end[1] = b'Z';
        past_end[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        assert_eq!(pe_machine_type(&past_end), None);

        let mut bad_sig = pe(IMAGE_FILE_MACHINE_AMD64);
        bad_sig[0x40..0x44].copy_from_slice(b"XXXX");
        assert_eq!(pe_machine_type(&bad_sig), None);
    }

    /// catalog:unit-request-update — auto-on one-shot: after finish the same
    /// desired git sha stays consumed; a later sha may begin.
    #[test]
    fn one_attempt_per_desired_sha() {
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let state = UpdateState::new();
        let sha_a = stub_desired("abc");
        let sha_b = stub_desired("def");
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&sha_a)).is_some(),
            "auto-on first desired git sha must apply"
        );
        state.finish();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&sha_a)).is_none(),
            "same desired git sha must stay consumed after finish"
        );
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&sha_b)).is_some(),
            "a later desired git sha may begin after finish"
        );
    }

    /// catalog:unit-request-update — mid-apply must not start a newer sha.
    #[test]
    fn maybe_queue_auto_auto_on_mid_apply_does_not_start_newer_sha() {
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&stub_desired("sha-a"))).is_some(),
            "auto-on first desired git sha must apply"
        );
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&stub_desired("sha-b"))).is_none(),
            "mid-apply must not start a newer sha"
        );
    }

    #[test]
    fn try_begin_empty_key_is_false_and_does_not_set_applying() {
        let state = UpdateState::new();
        assert!(!state.try_begin(""), "empty attempt key must not begin");
        assert!(
            state.try_begin("abc"),
            "empty-key reject must not leave applying set"
        );
    }

    #[test]
    fn attempt_key_prefers_trimmed_git_sha_else_sorted_bin_hashes() {
        let trimmed = DesiredSoftware {
            git_sha: "  abc  ".into(),
            platform: "linux-x86_64".into(),
            bins: HashMap::new(),
        };
        assert_eq!(attempt_key(&trimmed), "abc");

        let bins_only = DesiredSoftware {
            git_sha: "".into(),
            platform: "linux-x86_64".into(),
            bins: HashMap::from([
                (
                    "fleet-host".into(),
                    DesiredBin {
                        sha256: "bb".into(),
                        size: None,
                        version: None,
                    },
                ),
                (
                    "fleet-driver".into(),
                    DesiredBin {
                        sha256: "aa".into(),
                        size: None,
                        version: None,
                    },
                ),
            ]),
        };
        assert_eq!(attempt_key(&bins_only), "fleet-driver:aa,fleet-host:bb");
    }

    struct MockCp {
        base: String,
        log: Arc<StdMutex<Vec<(String, String)>>>,
        _join: thread::JoinHandle<()>,
    }

    fn spawn_mock(files: HashMap<String, Vec<u8>>) -> MockCp {
        spawn_mock_on_request(files, None)
    }

    fn spawn_mock_on_request(
        files: HashMap<String, Vec<u8>>,
        on_request: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    ) -> MockCp {
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
                if let Some(cb) = &on_request {
                    cb(&method, &path);
                }
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

    fn assert_apply_http_get_only_no_drain_paused(log: &[(String, String)]) {
        assert!(!log.is_empty(), "expected GET install bytes, got {log:?}");
        for (method, path) in log {
            assert_eq!(method, "GET", "apply must not {method} {path}");
            let lower = path.to_ascii_lowercase();
            assert!(
                !lower.contains("paused") && !lower.contains("drain"),
                "apply must not touch paused/drain via {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn apply_does_not_patch_drain_or_paused() {
        let dir = tempfile::tempdir().unwrap();
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-driver", old_d);
        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        // Non-empty jobs: ChildrenOnly must not wait_jobs_idle (that loop has no timeout).
        let jobs = AsyncMutex::new(HashSet::from([Uuid::now_v7()]));
        let state = UpdateState::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            apply_desired(
                &client,
                &cfg,
                &desired,
                dir.path(),
                &jobs,
                &state,
                &dry_apply_opts(),
            ),
        )
        .await
        .expect("children-only apply must not wait_jobs_idle")
        .expect("apply");
        assert!(!state.stop_claiming.load(Ordering::SeqCst));
        assert!(!state.is_updating());
        assert_apply_http_get_only_no_drain_paused(&mock.log.lock().unwrap());
    }

    #[tokio::test]
    async fn child_apply_stop_claiming_stays_false_during_download() {
        let dir = tempfile::tempdir().unwrap();
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-driver", old_d);
        let platform = detect_platform();
        let state = Arc::new(UpdateState::new());
        let saw_install_get = Arc::new(AtomicBool::new(false));
        let stop_on_first_get = Arc::new(AtomicBool::new(true));
        let mock = spawn_mock_on_request(
            files_for(
                &platform,
                &[
                    ("fleet-host", host.as_slice()),
                    ("fleet-driver", new_d.as_slice()),
                ],
            ),
            {
                let state = state.clone();
                let saw_install_get = saw_install_get.clone();
                let stop_on_first_get = stop_on_first_get.clone();
                Some(Arc::new(move |method: &str, path: &str| {
                    if method == "GET"
                        && path.contains("/install/worker/bin/")
                        && !saw_install_get.swap(true, Ordering::SeqCst)
                    {
                        stop_on_first_get.store(
                            state.stop_claiming.load(Ordering::SeqCst),
                            Ordering::SeqCst,
                        );
                    }
                }))
            },
        );
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            apply_desired(
                &client,
                &cfg,
                &desired,
                dir.path(),
                &AsyncMutex::new(HashSet::from([Uuid::now_v7()])),
                &state,
                &dry_apply_opts(),
            ),
        )
        .await
        .expect("children-only apply must not wait_jobs_idle")
        .expect("apply");
        assert!(
            saw_install_get.load(Ordering::SeqCst),
            "download must GET install so stop_claiming can be snapshotted"
        );
        assert!(
            !stop_on_first_get.load(Ordering::SeqCst),
            "children-only apply must keep claiming during download"
        );
    }

    #[tokio::test]
    async fn apply_desired_download_error_clears_updating_and_does_not_http_patch() {
        let dir = tempfile::tempdir().unwrap();
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";
        write_bin(dir.path(), "fleet-host", host);
        write_bin(dir.path(), "fleet-driver", old_d);
        let platform = detect_platform();
        let mock = spawn_mock(HashMap::new());
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            apply_desired(
                &client,
                &cfg,
                &desired,
                dir.path(),
                &AsyncMutex::new(HashSet::from([Uuid::now_v7()])),
                &state,
                &dry_apply_opts(),
            ),
        )
        .await
        .expect("download error must not wait_jobs_idle");
        match result {
            Ok(ApplyOutcome::QuietNoPrebuilt) | Err(_) => {}
            Ok(other) => panic!("expected QuietNoPrebuilt or err, got {other:?}"),
        }
        assert!(!state.is_updating());
        assert!(!state.stop_claiming.load(Ordering::SeqCst));
        assert_apply_http_get_only_no_drain_paused(&mock.log.lock().unwrap());
    }

    #[tokio::test]
    async fn host_apply_sets_updating_and_does_not_http_patch() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
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
        let log = mock.log.lock().unwrap();
        assert_apply_http_get_only_no_drain_paused(&log);
        for (_, path) in log.iter() {
            assert!(
                !path.contains("/api/worker-hosts/"),
                "host apply must not PATCH worker-hosts via {path}"
            );
        }
    }

    #[tokio::test]
    async fn host_apply_download_error_clears_updating() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        let platform = detect_platform();
        let mock = spawn_mock(HashMap::new());
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            apply_desired(
                &client,
                &cfg,
                &desired,
                dir.path(),
                &AsyncMutex::new(HashSet::new()),
                &state,
                &ApplyOptions {
                    windows_self_replace: true,
                    dry_restart: true,
                },
            ),
        )
        .await
        .expect("download error must not wait_jobs_idle");
        match result {
            Ok(ApplyOutcome::QuietNoPrebuilt) | Err(_) => {}
            Ok(other) => panic!("expected QuietNoPrebuilt or err, got {other:?}"),
        }
        assert_eq!(fs::read(dir.path().join("fleet-host")).unwrap(), old_h);
        assert!(!state.is_updating());
        assert!(!state.stop_claiming.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn windows_host_restart_rename_fail_writes_update_cmd_without_patch() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        let bak = dir.path().join("fleet-host.bak");
        fs::create_dir_all(&bak).unwrap();
        fs::write(bak.join("keep"), b"x").unwrap();
        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dir.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &ApplyOptions {
                windows_self_replace: true,
                dry_restart: true,
            },
        )
        .await
        .expect("windows rename-aside fail must fall back to update.cmd");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert!(dir.path().join("update.cmd").is_file());
        assert_eq!(fs::read(dir.path().join("fleet-host")).unwrap(), old_h);
        assert!(state.is_updating());
        assert_apply_http_get_only_no_drain_paused(&mock.log.lock().unwrap());
    }

    #[tokio::test]
    async fn host_apply_downloads_while_still_claiming() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        let platform = detect_platform();
        let state = Arc::new(UpdateState::new());
        let saw_install_get = Arc::new(AtomicBool::new(false));
        let stop_on_first_get = Arc::new(AtomicBool::new(true));
        let mock = spawn_mock_on_request(
            files_for(
                &platform,
                &[
                    ("fleet-host", new_h.as_slice()),
                    ("fleet-driver", d.as_slice()),
                ],
            ),
            {
                let state = state.clone();
                let saw_install_get = saw_install_get.clone();
                let stop_on_first_get = stop_on_first_get.clone();
                Some(Arc::new(move |method: &str, path: &str| {
                    if method == "GET"
                        && path.contains("/install/worker/bin/")
                        && !saw_install_get.swap(true, Ordering::SeqCst)
                    {
                        stop_on_first_get.store(
                            state.stop_claiming.load(Ordering::SeqCst),
                            Ordering::SeqCst,
                        );
                    }
                }))
            },
        );
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dir.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &ApplyOptions {
                windows_self_replace: true,
                dry_restart: true,
            },
        )
        .await
        .expect("apply");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert!(
            saw_install_get.load(Ordering::SeqCst),
            "download must GET install so stop_claiming can be snapshotted"
        );
        assert!(
            !stop_on_first_get.load(Ordering::SeqCst),
            "host apply must download while still claiming"
        );
        assert!(
            state.stop_claiming.load(Ordering::SeqCst),
            "stop_claiming flips only after stage"
        );
    }

    /// catalog:unit-request-update — local auto_update=false must not queue
    /// a CP-desired apply even when host.auto_update is true (box restrict).
    #[test]
    fn maybe_queue_auto_respects_local_flag() {
        let state = UpdateState::new();
        let cfg_off = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = stub_desired("sha1");
        assert!(
            maybe_queue_auto(&state, &cfg_off, &host, Some(&desired)).is_none(),
            "local auto off + CP auto on + no Update now must not queue"
        );
    }

    /// catalog:unit-request-update — host-bin swap / restart pending must
    /// not start another download.
    #[test]
    fn maybe_queue_auto_skips_while_host_updating() {
        let state = UpdateState::new();
        state.updating.store(true, Ordering::SeqCst);
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = stub_desired("sha1");
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&desired)).is_none(),
            "is_updating must skip queue even when local and CP auto are on"
        );
    }

    /// catalog:unit-request-update — apply sets updating at first download,
    /// before stop_claiming; a later Update now must not queue.
    #[test]
    fn maybe_queue_auto_skips_while_updating_before_stop_claiming() {
        let state = UpdateState::new();
        state.updating.store(true, Ordering::SeqCst);
        assert!(
            !state.stop_claiming.load(Ordering::SeqCst),
            "download-phase fixture: updating is set before stop_claiming"
        );
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:01Z")),
                Some(&stub_desired("sha-b"))
            )
            .is_none(),
            "is_updating must skip queue before stop_claiming flips"
        );
    }

    const EXISTING_MACHINE_TOKEN: &str = "flm_existing_pair_token";

    static CONFIG_DIR_LOCK: StdMutex<()> = StdMutex::new(());

    /// Points `token_path()` / `machine_id_path()` at a tempfile so apply
    /// cannot write the real `~/.config/fleet-host` pair files.
    struct IsolatedConfigDir {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        _dir: tempfile::TempDir,
    }

    impl IsolatedConfigDir {
        fn new() -> Self {
            let lock = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var("FLEET_HOST_CONFIG_DIR").ok();
            let dir = tempfile::tempdir().unwrap();
            unsafe {
                std::env::set_var("FLEET_HOST_CONFIG_DIR", dir.path());
            }
            Self {
                _lock: lock,
                prev,
                _dir: dir,
            }
        }
    }

    impl Drop for IsolatedConfigDir {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("FLEET_HOST_CONFIG_DIR", v),
                    None => std::env::remove_var("FLEET_HOST_CONFIG_DIR"),
                }
            }
        }
    }

    fn seed_pair_files(dir: &Path) -> (PathBuf, Vec<u8>, PathBuf, Vec<u8>) {
        let token = dir.join("machine.token");
        let id = dir.join("machine.id");
        let token_bytes = format!("{EXISTING_MACHINE_TOKEN}\n").into_bytes();
        let id_bytes = b"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\n".to_vec();
        fs::write(&token, &token_bytes).unwrap();
        fs::write(&id, &id_bytes).unwrap();
        (token, token_bytes, id, id_bytes)
    }

    fn seed_published_pair_files() -> (Vec<u8>, Vec<u8>) {
        let (_, token_bytes, _, id_bytes) = seed_pair_files(&crate::paths::config_dir());
        (token_bytes, id_bytes)
    }

    fn assert_pair_files_untouched(
        dir: &Path,
        token: &Path,
        token_bytes: &[u8],
        id: &Path,
        id_bytes: &[u8],
    ) {
        assert_eq!(
            fs::read(token).expect("machine.token must remain"),
            token_bytes,
            "update must not rewrite or rotate machine.token"
        );
        assert_eq!(
            fs::read(id).expect("machine.id must remain"),
            id_bytes,
            "update must not mint a new machine.id"
        );
        let mut tokens = Vec::new();
        collect_named(dir, "machine.token", &mut tokens);
        assert_eq!(
            tokens,
            vec![token.to_path_buf()],
            "update must not mint another machine.token: {tokens:?}"
        );
    }

    fn assert_published_pair_files_untouched(token_bytes: &[u8], id_bytes: &[u8]) {
        assert_pair_files_untouched(
            &crate::paths::config_dir(),
            &crate::paths::token_path(),
            token_bytes,
            &crate::paths::machine_id_path(),
            id_bytes,
        );
    }

    fn assert_no_pair_files(dir: &Path) {
        assert!(
            !dir.join("machine.token").exists(),
            "apply must not mint {}/machine.token",
            dir.display()
        );
        assert!(
            !dir.join("machine.id").exists(),
            "apply must not mint {}/machine.id",
            dir.display()
        );
        let mut tokens = Vec::new();
        collect_named(dir, "machine.token", &mut tokens);
        assert!(
            tokens.is_empty(),
            "apply must not mint machine.token under {}: {tokens:?}",
            dir.display()
        );
    }

    fn collect_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.file_name().is_some_and(|n| n == name) {
                out.push(p.clone());
            }
            if p.is_dir() {
                collect_named(&p, name, out);
            }
        }
    }

    fn assert_apply_http_does_not_rotate(log: &[(String, String)]) {
        assert!(!log.is_empty(), "expected GET install bytes, got {log:?}");
        for (method, path) in log {
            assert_eq!(method, "GET", "update must not {method} {path}");
            let lower = path.to_ascii_lowercase();
            assert!(
                !lower.contains("pair")
                    && !lower.contains("bootstrap")
                    && !lower.contains("rotate")
                    && !lower.contains("token")
                    && !lower.contains("/api/worker-hosts/"),
                "update must not mint or rotate a machine token via {method} {path}"
            );
        }
    }

    fn dry_apply_opts() -> ApplyOptions {
        ApplyOptions {
            windows_self_replace: false,
            dry_restart: true,
        }
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — auto-update child
    /// swap reuses the published machine.token / machine.id.
    #[tokio::test]
    async fn update_holds_ready_auto_and_now_do_not_rotate_machine_token() {
        let _cfg_dir = IsolatedConfigDir::new();
        let platform = detect_platform();
        let old_h = b"host-v1";
        let old_d = b"driver-v1";
        let new_d = b"driver-v2";

        let dest = tempfile::tempdir().unwrap();
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", old_d);
        let (token_bytes, id_bytes) = seed_published_pair_files();

        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", old_h.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        ));
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", old_h.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&desired)).is_some(),
            "auto-update on must queue apply"
        );
        let client =
            HostClient::with_machine_token(&mock.base, EXISTING_MACHINE_TOKEN, &cfg).unwrap();
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &dry_apply_opts(),
        )
        .await
        .expect("auto-update apply");
        assert_eq!(out, ApplyOutcome::ChildrenSwapped);
        assert_published_pair_files_untouched(&token_bytes, &id_bytes);
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — Update now host-bin
    /// swap reuses the published machine.token / machine.id.
    #[tokio::test]
    async fn update_now_host_bin_swap_does_not_rotate_machine_token() {
        let _cfg_dir = IsolatedConfigDir::new();
        let platform = detect_platform();
        let old_h = b"host-v1";
        let old_d = b"driver-v1";
        let new_h = b"host-v2";

        let dest = tempfile::tempdir().unwrap();
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", old_d);
        let (token_bytes, id_bytes) = seed_published_pair_files();

        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", old_d.as_slice()),
            ],
        ));
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let host = host_auto_off(Some("2026-01-01T00:00:00Z"));
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", old_d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&desired)).is_some(),
            "Update now must queue apply even when auto is off"
        );
        let client =
            HostClient::with_machine_token(&mock.base, EXISTING_MACHINE_TOKEN, &cfg).unwrap();
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &dry_apply_opts(),
        )
        .await
        .expect("update-now apply");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert_published_pair_files_untouched(&token_bytes, &id_bytes);
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — auto-update host-bin
    /// swap must not rotate the published pair files.
    #[tokio::test]
    async fn auto_update_host_bin_swap_does_not_rotate_machine_token() {
        let _cfg_dir = IsolatedConfigDir::new();
        let platform = detect_platform();
        let old_h = b"host-v1";
        let d = b"driver-same";
        let new_h = b"host-v2";

        let dest = tempfile::tempdir().unwrap();
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", d);
        let (token_bytes, id_bytes) = seed_published_pair_files();

        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig {
            auto_update: true,
            ..LocalConfig::default()
        };
        let host = HostPublic {
            auto_update: Some(true),
            ..HostPublic::default()
        };
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &host, Some(&desired)).is_some(),
            "auto-update on must queue a host-bin apply"
        );
        let client =
            HostClient::with_machine_token(&mock.base, EXISTING_MACHINE_TOKEN, &cfg).unwrap();
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &dry_apply_opts(),
        )
        .await
        .expect("auto-update host apply");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert_published_pair_files_untouched(&token_bytes, &id_bytes);
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — Windows rename-aside
    /// / update.cmd must not mint or rewrite the published pair files.
    #[tokio::test]
    async fn windows_self_replace_apply_does_not_rotate_machine_token() {
        let _cfg_dir = IsolatedConfigDir::new();
        let platform = detect_platform();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";

        let dest = tempfile::tempdir().unwrap();
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", d);
        let (token_bytes, id_bytes) = seed_published_pair_files();

        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client =
            HostClient::with_machine_token(&mock.base, EXISTING_MACHINE_TOKEN, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &ApplyOptions {
                windows_self_replace: true,
                dry_restart: true,
            },
        )
        .await
        .expect("windows host apply");
        assert_eq!(out, ApplyOutcome::HostRestartPending);
        assert_published_pair_files_untouched(&token_bytes, &id_bytes);
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — children swap must
    /// not write-if-missing pair files on dest or the published config path.
    #[tokio::test]
    async fn apply_desired_does_not_mint_pair_files_when_dest_has_none() {
        let _cfg_dir = IsolatedConfigDir::new();
        let platform = detect_platform();
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";

        let dest = tempfile::tempdir().unwrap();
        write_bin(dest.path(), "fleet-host", host);
        write_bin(dest.path(), "fleet-driver", old_d);
        assert_no_pair_files(dest.path());
        assert_no_pair_files(&crate::paths::config_dir());

        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        let out = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await
        .expect("children-only apply");
        assert_eq!(out, ApplyOutcome::ChildrenSwapped);
        assert_no_pair_files(dest.path());
        assert_no_pair_files(&crate::paths::config_dir());
        assert!(!crate::paths::token_path().exists());
        assert!(!crate::paths::machine_id_path().exists());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — a failed host swap
    /// must keep the paired token; do not mint a new one as the "fix".
    #[tokio::test]
    async fn update_holds_ready_mid_swap_does_not_mint_machine_token() {
        let _cfg_dir = IsolatedConfigDir::new();
        let dest = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let d = b"driver-same";
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", d);
        let (token_bytes, id_bytes) = seed_published_pair_files();
        // Occupied `.bak` makes `rename(dest, bak)` fail after stage.
        fs::create_dir_all(dest.path().join("fleet-host.bak")).unwrap();

        let platform = detect_platform();
        let new_h = b"host-new";
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client =
            HostClient::with_machine_token(&mock.base, EXISTING_MACHINE_TOKEN, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await
        .expect_err("occupied .bak must fail the host swap");
        assert_published_pair_files_untouched(&token_bytes, &id_bytes);
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — stage fail after
    /// download must not pair or rotate a machine token.
    #[tokio::test]
    async fn apply_desired_stage_fail_does_not_pair_or_rotate() {
        let _cfg_dir = IsolatedConfigDir::new();
        let dest = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let d = b"driver-same";
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", d);
        // Occupied `.new` makes `stage_new_file` fail after download.
        fs::create_dir_all(dest.path().join("fleet-host.new")).unwrap();

        let platform = detect_platform();
        let new_h = b"host-new";
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await
        .expect_err("occupied .new must fail stage");
        assert_no_pair_files(dest.path());
        assert_no_pair_files(&crate::paths::config_dir());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — children-only swap
    /// fail must not pair or rotate a machine token.
    #[tokio::test]
    async fn apply_desired_children_swap_fail_does_not_pair_or_rotate() {
        let _cfg_dir = IsolatedConfigDir::new();
        let dest = tempfile::tempdir().unwrap();
        let host = b"host-same";
        let old_d = b"driver-old";
        write_bin(dest.path(), "fleet-host", host);
        write_bin(dest.path(), "fleet-driver", old_d);
        fs::create_dir_all(dest.path().join("fleet-driver.bak")).unwrap();

        let platform = detect_platform();
        let new_d = b"driver-new";
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        ));
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", host.as_slice()),
                ("fleet-driver", new_d.as_slice()),
            ],
        );
        apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await
        .expect_err("occupied fleet-driver.bak must fail children swap");
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    /// catalog:unit-update-holds-ready-no-token-rotate — 404 install must
    /// not POST pair or bootstrap.
    #[tokio::test]
    async fn apply_desired_download_404_does_not_pair_or_rotate() {
        let _cfg_dir = IsolatedConfigDir::new();
        let dest = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let d = b"driver-same";
        write_bin(dest.path(), "fleet-host", old_h);
        write_bin(dest.path(), "fleet-driver", d);

        let platform = detect_platform();
        let new_h = b"host-new";
        let mock = spawn_mock(HashMap::new());
        let cfg = LocalConfig::default();
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let result = apply_desired(
            &client,
            &cfg,
            &desired,
            dest.path(),
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await;
        match result {
            Ok(ApplyOutcome::QuietNoPrebuilt) | Err(_) => {}
            Ok(other) => panic!("expected QuietNoPrebuilt or err, got {other:?}"),
        }
        assert_no_pair_files(dest.path());
        assert_apply_http_does_not_rotate(&mock.log.lock().unwrap());
    }

    fn install_get_count(log: &[(String, String)]) -> usize {
        log.iter()
            .filter(|(method, path)| method == "GET" && path.contains("/install/worker/bin/"))
            .count()
    }

    /// catalog:unit-request-update — host-bin apply leaves software.updating;
    /// finish() only clears applying, so a later Update now must not queue.
    #[test]
    fn update_now_mid_swap_does_not_keep_pulling_when_auto_off() {
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let desired = stub_desired("sha-a");
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:00Z")),
                Some(&desired)
            )
            .is_some(),
            "Update now must queue the host swap"
        );
        // Host-bin apply leaves updating set through exec/restart.
        state.updating.store(true, Ordering::SeqCst);
        state.finish();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:01Z")),
                Some(&stub_desired("sha-b"))
            )
            .is_none(),
            "after HostRestartPending + finish(), later Update now must not queue while is_updating"
        );
    }

    /// catalog:unit-request-update — a failed mid-swap consumes the one-shot.
    /// Auto-off must not keep retrying / pulling the leftover request.
    #[tokio::test]
    async fn update_now_failed_mid_swap_does_not_keep_pulling_when_auto_off() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        fs::create_dir_all(dir.path().join("fleet-host.bak")).unwrap();

        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let req = host_auto_off(Some("2026-01-01T00:00:00Z"));
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(&state, &cfg, &req, Some(&desired)).is_some(),
            "Update now must queue the apply that then fails mid-swap"
        );
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        let err = apply_desired(
            &client,
            &cfg,
            &desired,
            dir.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &dry_apply_opts(),
        )
        .await
        .expect_err("occupied .bak must fail the host swap");
        assert!(
            err.to_string().contains("rename") || err.to_string().contains("fleet-host"),
            "{err}"
        );
        assert_eq!(fs::read(dir.path().join("fleet-host")).unwrap(), old_h);
        assert!(!dir.path().join("fleet-host.new").is_file());
        assert!(!state.is_updating());
        assert!(!state.stop_claiming.load(Ordering::SeqCst));
        state.finish();

        let pulls = install_get_count(&mock.log.lock().unwrap());
        assert!(pulls > 0, "the failed apply must have fetched the new bin");
        let newer = {
            let mut d = desired.clone();
            d.git_sha = "newer-sha".into();
            d
        };
        assert!(
            maybe_queue_auto(&state, &cfg, &req, Some(&desired)).is_none(),
            "failed Update now must not retry the same request when auto is off"
        );
        assert!(
            maybe_queue_auto(&state, &cfg, &req, Some(&newer)).is_none(),
            "failed mid-swap must not keep pulling a newer sha for the leftover request"
        );
        assert_eq!(
            install_get_count(&mock.log.lock().unwrap()),
            pulls,
            "auto-off must not keep GET /install after a failed one-shot"
        );
    }

    /// catalog:unit-request-update — failed swap consumes the leftover stamp;
    /// a later Update now click must apply once updating is cleared.
    #[tokio::test]
    async fn update_now_after_failed_swap_later_click_applies() {
        let dir = tempfile::tempdir().unwrap();
        let old_h = b"host-old";
        let new_h = b"host-new";
        let d = b"driver-same";
        write_bin(dir.path(), "fleet-host", old_h);
        write_bin(dir.path(), "fleet-driver", d);
        fs::create_dir_all(dir.path().join("fleet-host.bak")).unwrap();

        let platform = detect_platform();
        let mock = spawn_mock(files_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        ));
        let cfg = LocalConfig {
            auto_update: false,
            ..LocalConfig::default()
        };
        let desired = desired_for(
            &platform,
            &[
                ("fleet-host", new_h.as_slice()),
                ("fleet-driver", d.as_slice()),
            ],
        );
        let state = UpdateState::new();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:00Z")),
                Some(&desired)
            )
            .is_some(),
            "Update now must queue the apply that then fails mid-swap"
        );
        let client = HostClient::for_pair(&mock.base, &cfg).unwrap();
        apply_desired(
            &client,
            &cfg,
            &desired,
            dir.path(),
            &AsyncMutex::new(HashSet::new()),
            &state,
            &dry_apply_opts(),
        )
        .await
        .expect_err("occupied .bak must fail the host swap");
        assert!(!state.is_updating());
        state.finish();
        assert!(
            maybe_queue_auto(
                &state,
                &cfg,
                &host_auto_off(Some("2026-01-01T00:00:01Z")),
                Some(&desired)
            )
            .is_some(),
            "a later Update now must apply once updating is cleared"
        );
    }
}
