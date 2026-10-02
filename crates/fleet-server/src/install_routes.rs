//! Public install scripts and optional prebuilt worker binaries.

use std::path::{Component, Path, PathBuf};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fleet_auth::{AppError, AppResult};
use fleet_db::WorkerSoftwareManifest;

use crate::state::AppState;

const INSTALL_WORKER_SH: &str = include_str!("../../../scripts/install-worker/install-worker.sh");
const INSTALL_WORKER_PS1: &str = include_str!("../../../scripts/install-worker/install-worker.ps1");
const UNIX_COMMON: &str = include_str!("../../../scripts/install-worker/_unix_common.sh");
const LINUX_X86_64: &str = include_str!("../../../scripts/install-worker/linux-x86_64.sh");
const LINUX_AARCH64: &str = include_str!("../../../scripts/install-worker/linux-aarch64.sh");
const LINUX_ARMV7: &str = include_str!("../../../scripts/install-worker/linux-armv7.sh");
const LINUX_X86_64_MUSL: &str =
    include_str!("../../../scripts/install-worker/linux-x86_64-musl.sh");
const LINUX_AARCH64_MUSL: &str =
    include_str!("../../../scripts/install-worker/linux-aarch64-musl.sh");
const LINUX_ARMV7_MUSL: &str = include_str!("../../../scripts/install-worker/linux-armv7-musl.sh");
const DARWIN_ARM64: &str = include_str!("../../../scripts/install-worker/darwin-arm64.sh");
const DARWIN_X86_64: &str = include_str!("../../../scripts/install-worker/darwin-x86_64.sh");
const FREEBSD_AMD64: &str = include_str!("../../../scripts/install-worker/freebsd-amd64.sh");
const WINDOWS_X86_64: &str = include_str!("../../../scripts/install-worker/windows-x86_64.ps1");
const WINDOWS_ARM64: &str = include_str!("../../../scripts/install-worker/windows-arm64.ps1");
const VERIFY_SH: &str = include_str!("../../../scripts/install-worker/verify.sh");
const VERIFY_PS1: &str = include_str!("../../../scripts/install-worker/verify.ps1");

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/install-worker.sh", get(get_install_worker_sh))
        .route("/install-worker.ps1", get(get_install_worker_ps1))
        .route(
            "/install/worker/bin/{platform}/{bin}",
            get(get_worker_bin).head(head_worker_bin),
        )
        .route("/install/worker/{*name}", get(get_named_script))
}

async fn get_install_worker_sh() -> Response {
    script_response("install-worker.sh", INSTALL_WORKER_SH)
}

async fn get_install_worker_ps1() -> Response {
    script_response("install-worker.ps1", INSTALL_WORKER_PS1)
}

async fn get_named_script(AxumPath(name): AxumPath<String>) -> AppResult<Response> {
    reject_unsafe_name(&name)?;
    let body = script_body(&name).ok_or_else(|| AppError::not_found("install script not found"))?;
    if name == "verify.sh" {
        let baked = bake_verify_origin(body, &verify_origin_from_env());
        return Ok(script_response(&name, &baked));
    }
    Ok(script_response(&name, body))
}

/// Piped `curl verify.sh | sh` has no sibling file. Stamp the pair origin
/// into the one token the script understands. `WORKER_PUBLIC_BASE` wins.
/// The API container often has only `API_PUBLIC_BASE` (compose does not
/// pass the worker base through). A git checkout stays unsubstituted so
/// `sh verify.sh` next to `_unix_common.sh` is unchanged.
fn verify_origin_from_env() -> String {
    for key in ["WORKER_PUBLIC_BASE", "API_PUBLIC_BASE"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim().trim_end_matches('/').to_string();
            if !value.is_empty() {
                return value;
            }
        }
    }
    String::new()
}

fn bake_verify_origin(body: &str, origin: &str) -> String {
    let origin = origin.trim().trim_end_matches('/').replace('\'', "");
    body.replace("'@fleet-verify-origin@'", &format!("'{origin}'"))
}

fn lookup_worker_bin(state: &AppState, platform: &str, bin: &str) -> AppResult<PathBuf> {
    reject_unsafe_name(platform)?;
    reject_unsafe_name(bin)?;
    if !is_allowed_worker_bin(bin) {
        return Err(AppError::not_found(missing_bin_hint(platform, bin)));
    }
    let exe_dir = current_exe_dir();
    let workspace_target = workspace_target_dir();
    resolve_worker_bin(
        state.worker_dist_dir.as_deref(),
        exe_dir.as_deref(),
        workspace_target.as_deref(),
        platform,
        bin,
        &native_platform_id(),
    )
    .ok_or_else(|| AppError::not_found(missing_bin_hint(platform, bin)))
}

async fn get_worker_bin(
    State(state): State<AppState>,
    AxumPath((platform, bin)): AxumPath<(String, String)>,
) -> AppResult<Response> {
    let ct = worker_bin_content_type(&bin);
    if let Some(hash) = synthesized_sha256_sidecar(&state, &platform, &bin) {
        return Ok((StatusCode::OK, [(header::CONTENT_TYPE, ct)], hash).into_response());
    }
    let path = lookup_worker_bin(&state, &platform, &bin)?;
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok((StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found(missing_bin_hint(&platform, &bin)))
        }
        Err(e) => Err(AppError::internal(e.to_string())),
    }
}

async fn head_worker_bin(
    State(state): State<AppState>,
    AxumPath((platform, bin)): AxumPath<(String, String)>,
) -> AppResult<Response> {
    let path = lookup_worker_bin(&state, &platform, &bin)?;
    match tokio::fs::metadata(&path).await {
        Ok(meta) => {
            let len = meta.len().to_string();
            let len = HeaderValue::from_str(&len).unwrap_or_else(|_| HeaderValue::from_static("0"));
            let ct = worker_bin_content_type(&bin);
            Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap()),
                    (header::CONTENT_LENGTH, len),
                ],
            )
                .into_response())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found(missing_bin_hint(&platform, &bin)))
        }
        Err(e) => Err(AppError::internal(e.to_string())),
    }
}

/// Staged host/driver names only. Unknown names 404 even if a file exists
/// under dist or workspace `target/` (so `fleet-server` is never a public GET).
fn is_allowed_worker_bin(bin: &str) -> bool {
    matches!(
        bin,
        "fleet-host"
            | "fleet-host.exe"
            | "fleet-driver"
            | "fleet-driver.exe"
            | "fleet-host.sha256"
            | "fleet-host.exe.sha256"
            | "fleet-driver.sha256"
            | "fleet-driver.exe.sha256"
            | "manifest.json"
            | "libunwind.dll"
            | "libunwind.dll.sha256"
    )
}

/// `FLEET_WORKER_DIST_DIR/{platform}/{bin}`, then `{exe_dir}/{platform}/{bin}`,
/// then (native only) `{exe_dir}/{bin}` and workspace `target/{release,debug}/{bin}`.
fn resolve_worker_bin(
    dist_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    workspace_target: Option<&Path>,
    platform: &str,
    bin: &str,
    native_platform: &str,
) -> Option<PathBuf> {
    if !is_allowed_worker_bin(bin) {
        return None;
    }
    if let Some(root) = dist_dir {
        let path = root.join(platform).join(bin);
        if path.starts_with(root) && path.is_file() {
            return Some(path);
        }
    }
    if let Some(dir) = exe_dir {
        let staged = dir.join(platform).join(bin);
        if staged.is_file() {
            return Some(staged);
        }
        if platform == native_platform {
            let sibling = dir.join(bin);
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    if platform == native_platform {
        if let Some(target) = workspace_target {
            for profile in ["release", "debug"] {
                let path = target.join(profile).join(bin);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// `CARGO_MANIFEST_DIR/../../target` (runtime env, else compile-time).
fn workspace_target_dir() -> Option<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").to_string());
    Some(PathBuf::from(manifest).join("../../target"))
}

/// Map this machine's uname to an install-worker platform id
/// (`linux`+`x86_64` → `linux-x86_64`, `Darwin`+`arm64` → `darwin-arm64`, …).
pub fn native_platform_id() -> String {
    let os = uname_s();
    let arch = uname_m();
    map_uname_platform(&os, &arch, is_musl())
}

fn map_uname_platform(os: &str, arch: &str, musl: bool) -> String {
    let os_lc = os.to_ascii_lowercase();
    let sys = match os_lc.as_str() {
        "linux" => "linux",
        "darwin" | "macos" => "darwin",
        "freebsd" => "freebsd",
        s if s.starts_with("mingw") || s.starts_with("msys") || s.starts_with("cygwin") => {
            "windows"
        }
        "windows" => "windows",
        other => other,
    };
    let cpu = match arch {
        "x86_64" | "amd64" | "x64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "armv7l" | "armv7" | "armhf" => "armv7",
        other => other,
    };
    match (sys, cpu, musl) {
        ("linux", "x86_64", true) => "linux-x86_64-musl".into(),
        ("linux", "aarch64", true) => "linux-aarch64-musl".into(),
        ("linux", "armv7", true) => "linux-armv7-musl".into(),
        ("linux", "x86_64", false) => "linux-x86_64".into(),
        ("linux", "aarch64", false) => "linux-aarch64".into(),
        ("linux", "armv7", false) => "linux-armv7".into(),
        ("darwin", "aarch64", _) => "darwin-arm64".into(),
        ("darwin", "x86_64", _) => "darwin-x86_64".into(),
        ("freebsd", "x86_64", _) => "freebsd-amd64".into(),
        ("windows", "x86_64", _) => "windows-x86_64".into(),
        ("windows", "aarch64", _) => "windows-arm64".into(),
        (sys, cpu, true) if sys == "linux" => format!("linux-{cpu}-musl"),
        (sys, cpu, _) => format!("{sys}-{cpu}"),
    }
}

fn uname_s() -> String {
    uname_arg("-s").unwrap_or_else(|| match std::env::consts::OS {
        "macos" => "Darwin".into(),
        other => other.to_string(),
    })
}

fn uname_m() -> String {
    uname_arg("-m").unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

fn uname_arg(flag: &str) -> Option<String> {
    let out = std::process::Command::new("uname")
        .arg(flag)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn is_musl() -> bool {
    if cfg!(target_env = "musl") {
        return true;
    }
    if Path::new("/etc/alpine-release").is_file() {
        return true;
    }
    if let Ok(out) = std::process::Command::new("ldd").arg("/bin/sh").output() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.to_ascii_lowercase().contains("musl") {
            return true;
        }
    }
    false
}

fn worker_bin_content_type(bin: &str) -> &'static str {
    if bin == "manifest.json" || bin.ends_with(".json") {
        "application/json"
    } else if bin.ends_with(".sha256") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

/// Staged `manifest.json` for this platform, or a hash of the bins this
/// process can already serve (API-image sibling / target/). Heartbeat
/// `desired` used to be None without a staged JSON, so auto-update never
/// ran even though GET /install/worker/bin/... returned the host binary.
pub fn load_platform_manifest(
    state: &AppState,
    platform: Option<&str>,
) -> Option<WorkerSoftwareManifest> {
    let platform = platform.map(str::trim).filter(|s| !s.is_empty())?;
    if reject_unsafe_name(platform).is_err() {
        return None;
    }
    if let Some(m) = load_manifest_file(state, platform) {
        return Some(m);
    }
    synthesize_manifest_from_bins(state, platform)
}

fn load_manifest_file(state: &AppState, platform: &str) -> Option<WorkerSoftwareManifest> {
    let path = resolve_worker_bin(
        state.worker_dist_dir.as_deref(),
        current_exe_dir().as_deref(),
        workspace_target_dir().as_deref(),
        platform,
        "manifest.json",
        &native_platform_id(),
    )?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                platform,
                path = %path.display(),
                error = %e,
                "worker dist manifest.json unreadable"
            );
            return None;
        }
    };
    match serde_json::from_slice::<WorkerSoftwareManifest>(&bytes) {
        Ok(mut m) => {
            if m.platform.trim().is_empty() {
                m.platform = platform.to_string();
            }
            Some(m)
        }
        Err(e) => {
            tracing::warn!(
                platform,
                path = %path.display(),
                error = %e,
                "worker dist manifest.json is not valid JSON"
            );
            None
        }
    }
}

fn synthesize_manifest_from_bins(
    state: &AppState,
    platform: &str,
) -> Option<WorkerSoftwareManifest> {
    let windows = platform.starts_with("windows-");
    let host_name = if windows {
        "fleet-host.exe"
    } else {
        "fleet-host"
    };
    let driver_name = if windows {
        "fleet-driver.exe"
    } else {
        "fleet-driver"
    };
    // An explicit dist dir is the only package. Do not treat cargo target
    // bins as staged when that directory has no manifest and no binaries.
    let exe_dir = current_exe_dir();
    let target_dir = workspace_target_dir();
    let (exe, target) = if state.worker_dist_dir.is_some() {
        (None, None)
    } else {
        (exe_dir.as_deref(), target_dir.as_deref())
    };
    let host_path = resolve_worker_bin(
        state.worker_dist_dir.as_deref(),
        exe,
        target,
        platform,
        host_name,
        &native_platform_id(),
    )?;
    let driver_path = resolve_worker_bin(
        state.worker_dist_dir.as_deref(),
        exe,
        target,
        platform,
        driver_name,
        &native_platform_id(),
    )?;
    let host_meta = bin_file_meta(&host_path)?;
    let driver_meta = bin_file_meta(&driver_path)?;
    let git_sha = option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let mut bins = std::collections::BTreeMap::new();
    bins.insert(host_name.to_string(), host_meta);
    bins.insert(driver_name.to_string(), driver_meta);
    Some(WorkerSoftwareManifest {
        git_sha,
        platform: platform.to_string(),
        staged_at: String::new(),
        bins,
    })
}

fn synthesized_sha256_sidecar(state: &AppState, platform: &str, bin: &str) -> Option<String> {
    let stem = bin.strip_suffix(".sha256")?;
    if lookup_worker_bin(state, platform, bin).is_ok() {
        return None;
    }
    let path = lookup_worker_bin(state, platform, stem).ok()?;
    let bytes = std::fs::read(path).ok()?;
    use sha2::{Digest, Sha256};
    Some(hex::encode(Sha256::digest(&bytes)))
}

fn bin_file_meta(path: &Path) -> Option<fleet_db::WorkerBinMeta> {
    use sha2::{Digest, Sha256};
    use std::sync::Mutex;
    use std::time::SystemTime;

    struct Entry {
        path: PathBuf,
        len: u64,
        modified: Option<SystemTime>,
        meta: fleet_db::WorkerBinMeta,
    }
    // Admin runner lists call this once per host. Debug fleet-host is hundreds
    // of MB, so a same-file hit must not re-read and hash.
    static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

    let fs_meta = std::fs::metadata(path).ok()?;
    let len = fs_meta.len();
    let modified = fs_meta.modified().ok();
    if let Ok(guard) = CACHE.lock() {
        if let Some(hit) = guard
            .iter()
            .find(|e| e.path == path && e.len == len && e.modified == modified)
        {
            return Some(hit.meta.clone());
        }
    }
    let bytes = std::fs::read(path).ok()?;
    let sha = hex::encode(Sha256::digest(&bytes));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let version = if name.starts_with("fleet-driver") {
        crate::package_versions::fleet_driver_package_version()
    } else {
        crate::package_versions::fleet_host_package_version()
    };
    let meta = fleet_db::WorkerBinMeta {
        sha256: sha,
        size: bytes.len() as u64,
        version: Some(version.to_string()),
    };
    if let Ok(mut guard) = CACHE.lock() {
        guard.push(Entry {
            path: path.to_path_buf(),
            len,
            modified,
            meta: meta.clone(),
        });
    }
    Some(meta)
}

/// Every staged `manifest.json` under `FLEET_WORKER_DIST_DIR` (one per platform).
pub fn list_dist_packages(state: &AppState) -> Vec<WorkerSoftwareManifest> {
    let Some(root) = state.worker_dist_dir.as_deref() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = ent.file_name();
        let platform = name.to_string_lossy();
        if reject_unsafe_name(&platform).is_err() {
            continue;
        }
        if let Some(m) = load_platform_manifest(state, Some(platform.as_ref())) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| a.platform.cmp(&b.platform));
    out
}

fn missing_bin_hint(platform: &str, bin: &str) -> String {
    format!(
        "binary not available for {platform}/{bin}; set FLEET_WORKER_DIST_DIR to a directory of {{platform}}/{{bin}} or build from source (FLEET_WORKER_BUILD_FROM_SOURCE=1)"
    )
}

fn script_response(name: &str, body: &str) -> Response {
    let ct = if name.ends_with(".sh") {
        HeaderValue::from_static("text/x-shellscript; charset=utf-8")
    } else {
        HeaderValue::from_static("text/plain; charset=utf-8")
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, ct)],
        body.to_string(),
    )
        .into_response()
}

/// Every script `GET /install/worker/{name}` can serve. Tests iterate this so a
/// new platform cannot silently start reading `API_PUBLIC_BASE`.
fn served_scripts() -> &'static [(&'static str, &'static str)] {
    &[
        ("install-worker.sh", INSTALL_WORKER_SH),
        ("install-worker.ps1", INSTALL_WORKER_PS1),
        ("_unix_common.sh", UNIX_COMMON),
        ("linux-x86_64.sh", LINUX_X86_64),
        ("linux-aarch64.sh", LINUX_AARCH64),
        ("linux-armv7.sh", LINUX_ARMV7),
        ("linux-x86_64-musl.sh", LINUX_X86_64_MUSL),
        ("linux-aarch64-musl.sh", LINUX_AARCH64_MUSL),
        ("linux-armv7-musl.sh", LINUX_ARMV7_MUSL),
        ("darwin-arm64.sh", DARWIN_ARM64),
        ("darwin-x86_64.sh", DARWIN_X86_64),
        ("freebsd-amd64.sh", FREEBSD_AMD64),
        ("windows-x86_64.ps1", WINDOWS_X86_64),
        ("windows-arm64.ps1", WINDOWS_ARM64),
        ("verify.sh", VERIFY_SH),
        ("verify.ps1", VERIFY_PS1),
    ]
}

fn script_body(name: &str) -> Option<&'static str> {
    served_scripts()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
}

fn reject_unsafe_name(name: &str) -> AppResult<()> {
    if name.is_empty() || name.contains('\0') {
        return Err(AppError::bad_request("invalid name"));
    }
    if name.contains("..") {
        return Err(AppError::bad_request("invalid name"));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(AppError::bad_request("invalid name"));
    }
    for c in path.components() {
        match c {
            Component::Normal(part) => {
                let s = part.to_string_lossy();
                if s.contains('/') || s.contains('\\') {
                    return Err(AppError::bad_request("invalid name"));
                }
            }
            Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_)
            | Component::RootDir => {
                return Err(AppError::bad_request("invalid name"));
            }
        }
    }
    if name.contains('/') || name.contains('\\') {
        return Err(AppError::bad_request("invalid name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_bin(dir: &Path, rel: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn bake_verify_origin_stamps_the_pair_url() {
        let body = "origin='@fleet-verify-origin@'\ncase @fleet-verify-origin@ in\n";
        let got = bake_verify_origin(body, "https://agentfleet.studio/");
        assert_eq!(
            got,
            "origin='https://agentfleet.studio'\ncase @fleet-verify-origin@ in\n"
        );
        assert_eq!(
            bake_verify_origin(body, ""),
            "origin=''\ncase @fleet-verify-origin@ in\n"
        );
    }

    #[test]
    fn map_uname_linux_x86_64() {
        let cases = [
            ("Linux", "x86_64", false, "linux-x86_64"),
            ("linux", "amd64", true, "linux-x86_64-musl"),
            ("Linux", "armv7l", false, "linux-armv7"),
            ("Linux", "armv7l", true, "linux-armv7-musl"),
            ("linux", "armhf", true, "linux-armv7-musl"),
            ("Darwin", "arm64", false, "darwin-arm64"),
            ("FreeBSD", "amd64", false, "freebsd-amd64"),
        ];
        for (os, arch, musl, want) in cases {
            assert_eq!(
                map_uname_platform(os, arch, musl),
                want,
                "map_uname_platform({os}, {arch}, {musl})"
            );
        }
    }

    #[test]
    fn map_uname_linux_aarch64_glibc_and_musl() {
        assert_eq!(
            map_uname_platform("Linux", "aarch64", false),
            "linux-aarch64"
        );
        assert_eq!(
            map_uname_platform("linux", "arm64", true),
            "linux-aarch64-musl"
        );
    }

    #[test]
    fn map_uname_windows_mingw_msys_to_windows_ids() {
        assert_eq!(
            map_uname_platform("MINGW64", "x86_64", false),
            "windows-x86_64"
        );
        assert_eq!(map_uname_platform("msys", "amd64", false), "windows-x86_64");
        assert_eq!(
            map_uname_platform("windows", "aarch64", false),
            "windows-arm64"
        );
    }

    #[test]
    fn map_uname_macos_and_x64_are_darwin_x86_64() {
        assert_eq!(
            map_uname_platform("macos", "x86_64", false),
            "darwin-x86_64"
        );
        assert_eq!(map_uname_platform("Darwin", "x64", false), "darwin-x86_64");
    }

    #[test]
    fn script_body_serves_linux_armv7_musl() {
        let body = script_body("linux-armv7-musl.sh").expect("wired in script_body");
        assert!(
            body.contains("FLEET_PLATFORM=linux-armv7-musl"),
            "linux-armv7-musl.sh must export FLEET_PLATFORM=linux-armv7-musl"
        );
    }

    #[test]
    fn script_body_serves_linux_armv7() {
        let body = script_body("linux-armv7.sh").expect("wired in script_body");
        assert!(
            body.contains("FLEET_PLATFORM=linux-armv7"),
            "linux-armv7.sh must export FLEET_PLATFORM=linux-armv7"
        );
        assert!(
            !body.contains("FLEET_PLATFORM=linux-armv7-musl"),
            "linux-armv7.sh must not export the musl id"
        );
    }

    #[test]
    fn served_unix_platform_scripts_export_matching_fleet_platform() {
        let mut stems = Vec::new();
        for (name, body) in served_scripts() {
            if !name.ends_with(".sh")
                || matches!(*name, "install-worker.sh" | "_unix_common.sh" | "verify.sh")
            {
                continue;
            }
            let stem = name
                .strip_suffix(".sh")
                .expect("unix platform wrapper ends in .sh");
            assert!(
                body.contains(&format!("FLEET_PLATFORM={stem}")),
                "{name} must export FLEET_PLATFORM={stem}"
            );
            stems.push(stem);
        }
        assert!(
            !stems.is_empty(),
            "served_scripts must include unix platform wrappers"
        );
    }

    #[tokio::test]
    async fn get_named_script_linux_armv7_musl_ok() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let app = Router::new().route("/install/worker/{*name}", get(get_named_script));
        let req = Request::builder()
            .uri("/install/worker/linux-armv7-musl.sh")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(
            body.contains("FLEET_PLATFORM=linux-armv7-musl"),
            "GET /install/worker/linux-armv7-musl.sh must export linux-armv7-musl"
        );
    }

    #[test]
    fn detect_ids_have_served_scripts() {
        let first_class = [
            map_uname_platform("Linux", "x86_64", false),
            map_uname_platform("Linux", "x86_64", true),
            map_uname_platform("Linux", "aarch64", false),
            map_uname_platform("Linux", "aarch64", true),
            map_uname_platform("Linux", "armv7l", false),
            map_uname_platform("Linux", "armv7l", true),
            map_uname_platform("Darwin", "arm64", false),
            map_uname_platform("Darwin", "x86_64", false),
            map_uname_platform("FreeBSD", "amd64", false),
            map_uname_platform("windows", "x86_64", false),
            map_uname_platform("windows", "aarch64", false),
        ];
        let served: Vec<&str> = served_scripts().iter().map(|(n, _)| *n).collect();
        for id in first_class {
            let sh = format!("{id}.sh");
            let ps1 = format!("{id}.ps1");
            assert!(
                served.contains(&sh.as_str()) || served.contains(&ps1.as_str()),
                "map_uname_platform first-class id {id} must have a served {id}.sh or {id}.ps1"
            );
        }
    }

    #[test]
    fn resolve_prefers_dist_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dist = write_bin(tmp.path(), "linux-x86_64/fleet-host", b"from-dist");
        let exe = tmp.path().join("exe");
        write_bin(&exe, "linux-x86_64/fleet-host", b"from-exe");
        let found = resolve_worker_bin(
            Some(tmp.path()),
            Some(&exe),
            None,
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(dist.as_path()));
    }

    #[test]
    fn resolve_exe_platform_then_native_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        let sibling = write_bin(&exe, "fleet-host", b"sibling");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(sibling.as_path()));
    }

    #[test]
    fn resolve_exe_native_sibling_none_for_foreign() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        write_bin(&exe, "fleet-host", b"sibling");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "darwin-arm64",
            "fleet-host",
            "linux-x86_64",
        );
        assert!(found.is_none());
    }

    #[test]
    fn resolve_exe_prefers_platform_subdir_over_native_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        let staged = write_bin(&exe, "linux-x86_64/fleet-host", b"staged");
        write_bin(&exe, "fleet-host", b"sibling");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(staged.as_path()));
    }

    #[test]
    fn resolve_exe_platform_subdir_hits_foreign() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        let staged = write_bin(&exe, "darwin-arm64/fleet-host", b"staged");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "darwin-arm64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(staged.as_path()));
    }

    #[test]
    fn resolve_exe_native_sibling_beats_workspace_target() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        let sibling = write_bin(&exe, "fleet-host", b"sibling");
        let target = tmp.path().join("target");
        write_bin(&target, "release/fleet-host", b"release");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            Some(&target),
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(sibling.as_path()));
    }

    #[test]
    fn resolve_workspace_target_release_then_debug_when_native() {
        let tmp = tempfile::tempdir().unwrap();
        write_bin(tmp.path(), "debug/fleet-host", b"debug");
        let release = write_bin(tmp.path(), "release/fleet-host", b"release");
        let found = resolve_worker_bin(
            None,
            None,
            Some(tmp.path()),
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(release.as_path()));
    }

    #[test]
    fn resolve_workspace_target_none_for_foreign() {
        let tmp = tempfile::tempdir().unwrap();
        write_bin(tmp.path(), "debug/fleet-host", b"debug");
        write_bin(tmp.path(), "release/fleet-host", b"release");
        let found = resolve_worker_bin(
            None,
            None,
            Some(tmp.path()),
            "windows-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert!(found.is_none());
    }

    #[test]
    fn resolve_workspace_target_debug_when_release_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let debug = write_bin(tmp.path(), "debug/fleet-host", b"debug");
        let found = resolve_worker_bin(
            None,
            None,
            Some(tmp.path()),
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(debug.as_path()));
    }

    #[test]
    fn reject_unsafe_name_rejects_empty_nul_dotdot_slash_and_absolute() {
        let rejected = ["", "\0", "..", "/etc/passwd", "a/b", "a\\b"];
        for name in rejected {
            let err = reject_unsafe_name(name).expect_err("unsafe token");
            match err {
                AppError::BadRequest(msg) => {
                    assert_eq!(msg, "invalid name", "name={name:?}");
                }
                other => panic!("name={name:?} must be BadRequest, got {other:?}"),
            }
        }
        reject_unsafe_name("linux-x86_64").expect("safe platform token");
        reject_unsafe_name("fleet-host").expect("safe bin token");
        reject_unsafe_name("linux-x86_64.sh").expect("safe script token");
    }

    struct PublicBaseProbe {
        success: bool,
        combined: String,
        requested_urls: String,
    }

    /// Run a served POSIX installer with both public bases in the child env.
    /// curl/wget/fetch are stubbed so a wrong origin is the URL log, not a
    /// real request (OAuth `API_PUBLIC_BASE` vs pair `WORKER_PUBLIC_BASE`).
    /// `fleet_url` / `fleet_install_origin` are optional child env; unset both
    /// is the fail-closed path (`_unix_common.sh` ORIGIN prefers install origin).
    fn probe_script_public_bases(
        script_name: &str,
        fleet_url: Option<&str>,
        fleet_install_origin: Option<&str>,
        worker_public_base: &str,
        api_public_base: &str,
    ) -> PublicBaseProbe {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let body =
            script_body(script_name).unwrap_or_else(|| panic!("{script_name} must be wired"));
        let script = tmp.path().join(script_name);
        fs::write(&script, body).unwrap();

        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let url_log = tmp.path().join("http.log");
        let stub = r#"#!/bin/sh
log=${FLEET_TEST_URL_LOG:?}
printf '%s\n' "$*" >> "$log"
for a in "$@"; do
  case $a in
    http://*|https://*) printf '%s\n' "$a" >> "${log}.urls" ;;
  esac
done
exit 22
"#;
        for name in ["curl", "wget", "fetch"] {
            let path = bin.join(name);
            fs::write(&path, stub).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let prefix = tmp.path().join("prefix");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        let path_var = format!("{}:/usr/bin:/bin", bin.display());
        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        cmd.env_clear();
        cmd.env("PATH", &path_var);
        cmd.env("HOME", &home);
        cmd.env("LC_ALL", "C");
        cmd.env("FLEET_PREFIX", &prefix);
        cmd.env("FLEET_HOST_CONFIG_DIR", home.join("cfg"));
        cmd.env("FLEET_TEST_URL_LOG", &url_log);
        cmd.env("WORKER_PUBLIC_BASE", worker_public_base);
        cmd.env("API_PUBLIC_BASE", api_public_base);
        cmd.env("FLEET_PAIR_TOKEN", "flp_testdeadbeef");
        if script_name != "install-worker.sh" {
            cmd.env("FLEET_PLATFORM", "test-platform");
        }
        match fleet_url {
            Some(url) => {
                cmd.env("FLEET_URL", url);
            }
            None => {
                cmd.env_remove("FLEET_URL");
            }
        }
        match fleet_install_origin {
            Some(url) => {
                cmd.env("FLEET_INSTALL_ORIGIN", url);
            }
            None => {
                cmd.env_remove("FLEET_INSTALL_ORIGIN");
            }
        }
        let output = cmd
            .output()
            .unwrap_or_else(|e| panic!("run served {script_name}: {e}"));
        let requested_urls =
            fs::read_to_string(format!("{}.urls", url_log.display())).unwrap_or_default();
        PublicBaseProbe {
            success: output.status.success(),
            combined: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            requested_urls,
        }
    }

    /// Served installers pair via FLEET_URL; none may read OAuth API_PUBLIC_BASE.
    #[test]
    fn install_scripts_use_worker_public_base_not_api_public_base() {
        for (name, body) in served_scripts() {
            assert!(
                !body.contains("API_PUBLIC_BASE"),
                "{name} must not reference API_PUBLIC_BASE (OAuth / public API)"
            );
            // verify.* diagnoses the local install and can restart it.
            // It does not pair. FLEET_URL is optional context for the ping.
            if name.starts_with("verify.") {
                continue;
            }
            assert!(
                body.contains("FLEET_URL"),
                "{name} must take the pair URL via FLEET_URL (WORKER_PUBLIC_BASE)"
            );
        }
    }

    /// Fetch origin is FLEET_URL. Distinct pair sentinels make a
    /// $WORKER_PUBLIC_BASE mutant visible; API_PUBLIC_BASE is OAuth.
    #[test]
    fn install_scripts_fetch_fleet_url_not_worker_public_base_env() {
        let fleet_url = "http://pair.example.test:9443";
        let worker_public_base = "http://other-pair.example.test:9555";
        let api_public_base = "http://oauth.example.test:8080";
        assert_ne!(fleet_url, worker_public_base);
        assert_ne!(fleet_url, api_public_base);
        assert_ne!(worker_public_base, api_public_base);

        for name in [
            "install-worker.sh",
            "linux-x86_64.sh",
            "darwin-arm64.sh",
            "freebsd-amd64.sh",
        ] {
            let probe = probe_script_public_bases(
                name,
                Some(fleet_url),
                None,
                worker_public_base,
                api_public_base,
            );
            assert!(
                probe
                    .requested_urls
                    .lines()
                    .any(|u| u.starts_with(fleet_url)),
                "{name} must fetch against FLEET_URL={fleet_url} \
                 (echoing FLEET_URL is not enough):\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
            assert!(
                !probe
                    .requested_urls
                    .lines()
                    .any(|u| u.starts_with(worker_public_base)),
                "{name} fetched WORKER_PUBLIC_BASE instead of FLEET_URL:\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
            assert!(
                !probe
                    .requested_urls
                    .lines()
                    .any(|u| u.starts_with(api_public_base)),
                "{name} fetched API_PUBLIC_BASE (OAuth) instead of FLEET_URL:\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
        }
    }

    /// Unset FLEET_URL must fail closed and must not fetch either public base.
    #[test]
    fn install_wrapper_unset_fleet_url_fails_closed() {
        let worker_public_base = "http://pair.example.test:9443";
        let api_public_base = "http://oauth.example.test:8080";
        let probe = probe_script_public_bases(
            "linux-x86_64.sh",
            None,
            None,
            worker_public_base,
            api_public_base,
        );
        assert!(
            !probe.success,
            "unset FLEET_URL must fail closed: {}",
            probe.combined
        );
        assert!(
            !probe
                .requested_urls
                .lines()
                .any(|u| u.starts_with(api_public_base) || u.starts_with(worker_public_base)),
            "unset FLEET_URL must fetch neither API_PUBLIC_BASE nor WORKER_PUBLIC_BASE: {}",
            probe.requested_urls
        );
    }

    struct InstallWorkerChildEnv {
        success: bool,
        combined: String,
        child_env: String,
        requested_urls: String,
    }

    fn child_env_field<'a>(dump: &'a str, key: &str) -> Option<&'a str> {
        let prefix = format!("{key}=");
        dump.lines().find_map(|line| line.strip_prefix(&prefix))
    }

    /// Dump written by the stub platform / `_unix_common.sh` child.
    const CHILD_ENV_DUMP_SH: &str = r#"#!/bin/sh
{
  printf 'FLEET_PAIR_TOKEN=%s\n' "${FLEET_PAIR_TOKEN-}"
  printf 'FLEET_URL=%s\n' "${FLEET_URL-}"
  printf 'FLEET_INSTALL_ORIGIN=%s\n' "${FLEET_INSTALL_ORIGIN-}"
} > "${FLEET_TEST_CHILD_ENV:?}"
"#;

    /// Run served `install-worker.sh`. Stub curl/wget/fetch so `http_get`
    /// writes `platform_body` (the `sh "$tmp"` child). `unix_common_body`
    /// is the wrapper's curl|sh of `_unix_common.sh` when that hop runs.
    fn run_install_worker_bootstrap(
        fleet_url: &str,
        pair_token: &str,
        platform_body: &str,
        unix_common_body: Option<&str>,
    ) -> InstallWorkerChildEnv {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("install-worker.sh");
        fs::write(
            &script,
            script_body("install-worker.sh").expect("unix install command"),
        )
        .unwrap();

        let platform_path = tmp.path().join("platform.sh");
        fs::write(&platform_path, platform_body).unwrap();
        let common_path = tmp.path().join("unix_common.sh");
        if let Some(common) = unix_common_body {
            fs::write(&common_path, common).unwrap();
        }

        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let url_log = tmp.path().join("http.log");
        let stub = r#"#!/bin/sh
log=${FLEET_TEST_URL_LOG:?}
printf '%s\n' "$*" >> "$log"
url=""
for a in "$@"; do
  case $a in
    http://*|https://*) url=$a; printf '%s\n' "$a" >> "${log}.urls" ;;
  esac
done
case $url in
  *_unix_common.sh*)
    if [ -n "${FLEET_TEST_UNIX_COMMON_BODY:-}" ]; then
      cat "$FLEET_TEST_UNIX_COMMON_BODY"
      exit 0
    fi
    exit 22
    ;;
esac
cat "${FLEET_TEST_PLATFORM_BODY:?}"
exit 0
"#;
        for name in ["curl", "wget", "fetch"] {
            let path = bin.join(name);
            fs::write(&path, stub).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let child_env_log = tmp.path().join("child.env");
        let path_var = format!("{}:/usr/bin:/bin", bin.display());
        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        cmd.env_clear();
        cmd.env("PATH", &path_var);
        cmd.env("HOME", &home);
        cmd.env("LC_ALL", "C");
        cmd.env("TMPDIR", tmp.path());
        cmd.env("FLEET_TEST_URL_LOG", &url_log);
        cmd.env("FLEET_TEST_PLATFORM_BODY", &platform_path);
        cmd.env("FLEET_TEST_CHILD_ENV", &child_env_log);
        cmd.env("FLEET_URL", fleet_url);
        cmd.env("FLEET_PAIR_TOKEN", pair_token);
        cmd.env_remove("FLEET_INSTALL_ORIGIN");
        cmd.env_remove("FLEET_PLATFORM");
        if unix_common_body.is_some() {
            cmd.env("FLEET_TEST_UNIX_COMMON_BODY", &common_path);
        } else {
            cmd.env_remove("FLEET_TEST_UNIX_COMMON_BODY");
        }
        let output = cmd
            .output()
            .unwrap_or_else(|e| panic!("run served install-worker.sh: {e}"));
        InstallWorkerChildEnv {
            success: output.status.success(),
            combined: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            child_env: fs::read_to_string(&child_env_log).unwrap_or_default(),
            requested_urls: fs::read_to_string(format!("{}.urls", url_log.display()))
                .unwrap_or_default(),
        }
    }

    /// UI one-liner sets FLEET_URL / FLEET_PAIR_TOKEN on the bootstrap sh.
    /// After normalize, the platform child (`sh "$tmp"`) must see the pair
    /// token plus FLEET_URL / FLEET_INSTALL_ORIGIN equal to that origin.
    #[test]
    fn install_worker_sh_exports_fleet_pair_token_and_fleet_url() {
        let raw_url = "http://pair.example.test:9443/";
        let normalized = "http://pair.example.test:9443";
        let token = "flp_testdeadbeef";
        assert_ne!(raw_url, normalized);

        let probe = run_install_worker_bootstrap(raw_url, token, CHILD_ENV_DUMP_SH, None);
        assert!(
            probe.success,
            "bootstrap must run the platform child: {}",
            probe.combined
        );
        assert!(
            !probe.child_env.is_empty(),
            "platform child must dump env (http_get / sh \"$tmp\" never ran): {}",
            probe.combined
        );
        assert_eq!(
            child_env_field(&probe.child_env, "FLEET_PAIR_TOKEN"),
            Some(token),
            "platform child must inherit FLEET_PAIR_TOKEN: {}",
            probe.child_env
        );
        assert_eq!(
            child_env_field(&probe.child_env, "FLEET_URL"),
            Some(normalized),
            "platform child FLEET_URL must be the normalized origin, not {raw_url}: {}",
            probe.child_env
        );
        assert_eq!(
            child_env_field(&probe.child_env, "FLEET_INSTALL_ORIGIN"),
            Some(normalized),
            "platform child FLEET_INSTALL_ORIGIN must be the normalized origin: {}",
            probe.child_env
        );
    }

    /// Windows bootstrap must put the normalized origin on $env:FLEET_URL and
    /// $env:FLEET_INSTALL_ORIGIN before the downloaded platform .ps1 runs.
    #[test]
    fn install_worker_ps1_sets_env_fleet_url_and_install_origin_before_platform_invoke() {
        let boot = script_body("install-worker.ps1").expect("windows bootstrap");
        let set_url = boot
            .find("$env:FLEET_URL = $origin")
            .expect("install-worker.ps1 must set $env:FLEET_URL to the normalized origin");
        let set_origin = boot.find("$env:FLEET_INSTALL_ORIGIN = $origin").expect(
            "install-worker.ps1 must set $env:FLEET_INSTALL_ORIGIN to the normalized origin",
        );
        let invoke = boot
            .find("[scriptblock]::Create")
            .expect("bootstrap must invoke the downloaded platform .ps1");
        assert!(
            set_url < invoke,
            "$env:FLEET_URL must equal the normalized origin before the platform .ps1 runs"
        );
        assert!(
            set_origin < invoke,
            "$env:FLEET_INSTALL_ORIGIN must equal the normalized origin before the platform .ps1 runs"
        );
        assert!(
            boot.contains("$origin = Normalize-Url"),
            "origin assigned to $env:FLEET_URL / $env:FLEET_INSTALL_ORIGIN must be Normalize-Url'd"
        );
    }

    /// After `sh "$tmp"`, the platform wrapper's curl|sh of `_unix_common.sh`
    /// is a new sh and must still see FLEET_PAIR_TOKEN.
    #[test]
    fn install_worker_sh_two_hop_unix_common_inherits_pair_token() {
        let token = "flp_testdeadbeef";
        let wrapper = script_body("linux-x86_64.sh").expect("unix platform wrapper");
        let probe = run_install_worker_bootstrap(
            "http://pair.example.test:9443",
            token,
            wrapper,
            Some(CHILD_ENV_DUMP_SH),
        );
        assert!(
            probe.requested_urls.contains("_unix_common.sh"),
            "second hop must fetch _unix_common.sh:\nurls={}\nout={}",
            probe.requested_urls,
            probe.combined
        );
        assert!(
            !probe.child_env.is_empty(),
            "piped _unix_common.sh child must dump env: {}",
            probe.combined
        );
        assert_eq!(
            child_env_field(&probe.child_env, "FLEET_PAIR_TOKEN"),
            Some(token),
            "piped _unix_common.sh must inherit FLEET_PAIR_TOKEN: {}",
            probe.child_env
        );
    }

    /// Unset FLEET_URL / FLEET_INSTALL_ORIGIN must fail closed. Sourced
    /// API_PUBLIC_BASE / WORKER_PUBLIC_BASE must not become the pair origin.
    #[test]
    fn install_worker_unix_hits_worker_public_base_not_api_public_base() {
        let worker_public_base = "http://pair.example.test:9443";
        let api_public_base = "http://oauth.example.test:8080";
        assert_ne!(worker_public_base, api_public_base);

        let missing = probe_script_public_bases(
            "_unix_common.sh",
            None,
            None,
            worker_public_base,
            api_public_base,
        );
        assert!(
            !missing.success,
            "unset FLEET_URL must fail closed: {}",
            missing.combined
        );
        assert!(
            missing.requested_urls.is_empty(),
            "unset FLEET_URL must not fetch API_PUBLIC_BASE or WORKER_PUBLIC_BASE: {}",
            missing.requested_urls
        );
        assert!(
            !missing.combined.contains(api_public_base),
            "must not fall back to API_PUBLIC_BASE when FLEET_URL is unset: {}",
            missing.combined
        );
    }

    /// Bootstrap exports FLEET_INSTALL_ORIGIN; ORIGIN prefers it over FLEET_URL.
    /// FLEET_URL unset must still fetch that origin and never the OAuth base.
    #[test]
    fn unix_common_fleet_install_origin_alone_hits_that_origin_not_api_public_base() {
        let install_origin = "http://pair.example.test:9443";
        let worker_public_base = "http://other-pair.example.test:9555";
        let api_public_base = "http://oauth.example.test:8080";
        assert_ne!(install_origin, worker_public_base);
        assert_ne!(install_origin, api_public_base);

        let probe = probe_script_public_bases(
            "_unix_common.sh",
            None,
            Some(install_origin),
            worker_public_base,
            api_public_base,
        );
        assert!(
            probe
                .requested_urls
                .lines()
                .any(|u| u.starts_with(install_origin)),
            "_unix_common.sh must fetch against FLEET_INSTALL_ORIGIN={install_origin} \
             when FLEET_URL is unset:\nurls={}\nout={}",
            probe.requested_urls,
            probe.combined
        );
        assert!(
            !probe
                .requested_urls
                .lines()
                .any(|u| u.starts_with(api_public_base)),
            "_unix_common.sh fetched API_PUBLIC_BASE (OAuth) instead of FLEET_INSTALL_ORIGIN:\nurls={}\nout={}",
            probe.requested_urls,
            probe.combined
        );
        assert!(
            !probe.combined.contains(api_public_base),
            "must not fall through to API_PUBLIC_BASE: {}",
            probe.combined
        );
    }

    struct UnixCommonRun {
        success: bool,
        combined: String,
        wrote_bins: bool,
    }

    /// Run the served `_unix_common.sh` in an isolated prefix. Platform is not
    /// `linux-*` so a missing token cannot `systemctl stop` a host unit.
    /// `127.0.0.1` is fine for fail-closed token-gate cases (they die first);
    /// a present token must use a non-loopback closed origin or
    /// `warn_loopback_origin` dies before mkdir/fetch.
    fn run_unix_common_install(pair_token: Option<&str>) -> UnixCommonRun {
        run_unix_common_install_at(pair_token, "http://127.0.0.1:1", None)
    }

    fn run_unix_common_install_at(
        pair_token: Option<&str>,
        fleet_url: &str,
        leftover_machine_token: Option<&str>,
    ) -> UnixCommonRun {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("_unix_common.sh");
        fs::write(&script, script_body("_unix_common.sh").expect("wired")).unwrap();
        let prefix = tmp.path().join("prefix");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let cfg = home.join("cfg");
        if let Some(contents) = leftover_machine_token {
            fs::create_dir_all(&cfg).unwrap();
            fs::write(cfg.join("machine.token"), contents).unwrap();
        }

        // Stub fetchers so a non-loopback closed origin fails immediately
        // (no DNS/connect hang) after the token gate.
        let stub_bin = tmp.path().join("stub-bin");
        fs::create_dir_all(&stub_bin).unwrap();
        let stub = "#!/bin/sh\nexit 22\n";
        for name in ["curl", "wget", "fetch"] {
            let path = stub_bin.join(name);
            fs::write(&path, stub).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        cmd.env_clear();
        cmd.env("PATH", format!("{}:/usr/bin:/bin", stub_bin.display()));
        cmd.env("HOME", &home);
        cmd.env("FLEET_PREFIX", &prefix);
        cmd.env("FLEET_URL", fleet_url);
        cmd.env("FLEET_PLATFORM", "test-platform");
        cmd.env("FLEET_HOST_CONFIG_DIR", &cfg);
        cmd.env("LC_ALL", "C");
        match pair_token {
            Some(tok) => {
                cmd.env("FLEET_PAIR_TOKEN", tok);
            }
            None => {
                cmd.env_remove("FLEET_PAIR_TOKEN");
            }
        }
        let output = cmd.output().expect("run served _unix_common.sh");
        let bin = prefix.join("bin");
        let wrote_bins = [
            "fleet-host",
            "fleet-driver",
            "fleet-host.tmp",
            "fleet-driver.tmp",
        ]
        .iter()
        .any(|name| bin.join(name).exists())
            || bin.is_dir()
                && fs::read_dir(&bin).ok().is_some_and(|rd| {
                    rd.filter_map(|e| e.ok()).any(|e| {
                        let n = e.file_name();
                        let n = n.to_string_lossy();
                        n.contains("fleet-host") || n.contains("fleet-driver")
                    })
                });
        UnixCommonRun {
            success: output.status.success(),
            combined: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            wrote_bins,
        }
    }

    fn assert_missing_token_fail_closed(run: &UnixCommonRun) {
        assert!(
            !run.success,
            "missing/empty FLEET_PAIR_TOKEN must exit non-zero: {}",
            run.combined
        );
        assert!(
            run.combined.contains("FLEET_PAIR_TOKEN"),
            "fail-closed error must name FLEET_PAIR_TOKEN, got: {}",
            run.combined
        );
        assert!(
            !run.wrote_bins,
            "must not write fleet-host/fleet-driver when the pair token is missing: {}",
            run.combined
        );
        assert!(
            !run.combined.contains("downloading fleet-host")
                && !run.combined.contains("checking fleet-host")
                && !run.combined.contains("no test-platform bins"),
            "must fail closed before fetching or writing bins: {}",
            run.combined
        );
    }

    #[test]
    fn install_worker_missing_pair_token_fails_closed_before_writing_bins() {
        assert_missing_token_fail_closed(&run_unix_common_install(None));
    }

    #[test]
    fn install_worker_empty_pair_token_fails_closed_before_writing_bins() {
        assert_missing_token_fail_closed(&run_unix_common_install(Some("")));
    }

    #[test]
    fn install_worker_whitespace_pair_token_fails_closed_before_writing_bins() {
        assert_missing_token_fail_closed(&run_unix_common_install(Some(" \t")));
    }

    /// A non-empty pair token must pass the require-token gate. Closed
    /// non-loopback origin then dies on bins (loopback would die in
    /// `warn_loopback_origin` first).
    #[test]
    fn install_worker_present_pair_token_passes_token_gate() {
        let run = run_unix_common_install_at(
            Some("flp_testdeadbeef"),
            "http://pair.example.test:1",
            None,
        );
        assert!(
            !run.success,
            "closed origin must not install bins: {}",
            run.combined
        );
        assert!(
            !run.wrote_bins,
            "must not write fleet-host/fleet-driver from a closed origin: {}",
            run.combined
        );
        assert!(
            run.combined
                .contains("no test-platform bins from http://pair.example.test:1"),
            "set FLEET_PAIR_TOKEN must pass the token gate and fail on bins/origin: {}",
            run.combined
        );
    }

    /// Leftover non-empty machine.token is already_paired: unset
    /// FLEET_PAIR_TOKEN must still pass the token gate.
    #[test]
    fn install_worker_already_paired_machine_token_allows_missing_pair_token() {
        let run =
            run_unix_common_install_at(None, "http://pair.example.test:1", Some("flm_existing"));
        assert!(
            !run.success,
            "closed origin must not install bins: {}",
            run.combined
        );
        assert!(
            !run.wrote_bins,
            "must not write fleet-host/fleet-driver from a closed origin: {}",
            run.combined
        );
        assert!(
            run.combined
                .contains("no test-platform bins from http://pair.example.test:1"),
            "leftover machine.token must pass the token gate without FLEET_PAIR_TOKEN: {}",
            run.combined
        );
    }

    /// Leftover whitespace-only machine.token is unpaired (same as
    /// MachineToken::parse) and must fail closed like a missing token.
    #[test]
    fn install_worker_whitespace_machine_token_file_still_requires_pair_token() {
        assert_missing_token_fail_closed(&run_unix_common_install_at(
            None,
            "http://127.0.0.1:1",
            Some(" \t"),
        ));
    }

    /// Leftover empty machine.token is unpaired; omit FLEET_PAIR_TOKEN.
    #[test]
    fn install_worker_leftover_empty_machine_token_fails_closed_before_writing_bins() {
        assert_missing_token_fail_closed(&run_unix_common_install_at(
            None,
            "http://127.0.0.1:1",
            Some(""),
        ));
    }

    /// Leftover " \t\n" machine.token trims to empty (same unpaired gate).
    #[test]
    fn install_worker_leftover_whitespace_newline_machine_token_fails_closed_before_writing_bins() {
        assert_missing_token_fail_closed(&run_unix_common_install_at(
            None,
            "http://127.0.0.1:1",
            Some(" \t\n"),
        ));
    }

    fn powershell_function_body<'a>(src: &'a str, name: &str) -> &'a str {
        let header = format!("function {name}");
        let start = src
            .find(&header)
            .unwrap_or_else(|| panic!("{name} must be defined"));
        let rest = &src[start..];
        let end = rest[header.len()..]
            .find("\nfunction ")
            .map(|i| header.len() + i)
            .unwrap_or(rest.len());
        &rest[..end]
    }

    fn windows_bootstrap_platform_script_url_offset(boot: &str) -> Option<usize> {
        boot.find(r#"$origin/install/worker/${platform}.ps1"#)
            .or_else(|| boot.find(r#"$origin/install/worker/$platform.ps1"#))
    }

    fn windows_first_install_pair_token_throw_offset(body: &str) -> Option<usize> {
        body.find("FLEET_PAIR_TOKEN is not set")
    }

    /// x64 PowerShell on ARM64 reports AMD64 for this process. Get-FleetPlatform
    /// must read Machine PROCESSOR_ARCHITECTURE first.
    #[test]
    fn install_worker_ps1_selects_machine_arch() {
        for name in [
            "install-worker.ps1",
            "windows-x86_64.ps1",
            "windows-arm64.ps1",
        ] {
            let body = script_body(name).expect("wired");
            let detect = powershell_function_body(body, "Get-FleetPlatform");
            let machine = detect
                .find(r#"GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")"#)
                .unwrap_or_else(|| {
                    panic!(
                        "{name} Get-FleetPlatform must read Machine PROCESSOR_ARCHITECTURE \
                         (x64 PowerShell on ARM64 Windows reports AMD64)"
                    )
                });
            if let Some(process) = detect.find("$env:PROCESSOR_ARCHITECTURE") {
                assert!(
                    machine < process,
                    "{name} must prefer Machine PROCESSOR_ARCHITECTURE over this process"
                );
            }
        }
    }

    /// Bootstrap must fetch the detected platform script, never a pinned
    /// windows-x86_64 / windows-arm64 URL (that PE is the other arch).
    #[test]
    fn install_worker_ps1_does_not_hardcode_platform_script_url() {
        let boot = script_body("install-worker.ps1").expect("windows bootstrap");
        assert!(
            windows_bootstrap_platform_script_url_offset(boot).is_some(),
            "install-worker.ps1 must download the platform script for the detected machine"
        );
        assert!(
            !boot.contains("/install/worker/windows-x86_64.ps1")
                && !boot.contains("/install/worker/windows-arm64.ps1")
                && !boot.contains("/install/worker/bin/windows-x86_64")
                && !boot.contains("/install/worker/bin/windows-arm64"),
            "install-worker.ps1 must not hardcode an arch URL; a pinned path downloads the other PE"
        );
    }

    /// FLEET_DETECT_ONLY=1 must return before fetching the platform installer.
    #[test]
    fn install_worker_ps1_detect_only_returns_before_platform_download() {
        let boot = script_body("install-worker.ps1").expect("windows bootstrap");
        let detect_at = boot
            .find("FLEET_DETECT_ONLY")
            .expect("install-worker.ps1 must honor FLEET_DETECT_ONLY");
        let return_at = boot[detect_at..]
            .find("return")
            .map(|i| detect_at + i)
            .expect("FLEET_DETECT_ONLY=1 must return before downloading the platform script");
        let url_at = windows_bootstrap_platform_script_url_offset(boot)
            .expect("install-worker.ps1 must download $origin/install/worker/${platform}.ps1");
        assert!(
            return_at < url_at,
            "FLEET_DETECT_ONLY=1 return must precede the platform-script download"
        );
    }

    /// First-install missing/empty/whitespace FLEET_PAIR_TOKEN must throw
    /// before creating BIN_DIR or downloading bins.
    #[test]
    fn install_worker_windows_pair_token_required_before_writing_bins() {
        for name in ["windows-x86_64.ps1", "windows-arm64.ps1"] {
            let body = script_body(name).expect("wired");
            let token_at =
                windows_first_install_pair_token_throw_offset(body).unwrap_or_else(|| {
                    panic!("{name} must fail closed when FLEET_PAIR_TOKEN is missing")
                });
            let mkdir_at = body
                .find("New-Item")
                .expect("platform script creates BIN_DIR");
            let download_at = body
                .find("downloading $bin")
                .expect("platform script downloads fleet-host / fleet-driver");
            assert!(
                token_at < mkdir_at,
                "{name} empty FLEET_PAIR_TOKEN must throw before creating BIN_DIR"
            );
            assert!(
                token_at < download_at,
                "{name} empty FLEET_PAIR_TOKEN must throw before downloading bins"
            );
            let pair_at = body
                .find("FLEET_PAIR_TOKEN")
                .unwrap_or_else(|| panic!("{name} must read FLEET_PAIR_TOKEN"));
            assert!(
                body[pair_at..token_at].contains(".Trim()"),
                "{name} must Trim FLEET_PAIR_TOKEN so whitespace-only is empty"
            );
        }
    }

    /// Leftover non-empty $FLEET_HOST_CONFIG_DIR/machine.token is already
    /// paired: unset FLEET_PAIR_TOKEN must not take the first-install throw.
    #[test]
    fn install_worker_windows_already_paired_allows_missing_pair_token() {
        for name in ["windows-x86_64.ps1", "windows-arm64.ps1"] {
            let body = script_body(name).expect("wired");
            let throw_at = windows_first_install_pair_token_throw_offset(body)
                .unwrap_or_else(|| panic!("{name} must throw on first-install missing pair token"));
            let before = &body[..throw_at];
            assert!(
                before.contains("FLEET_HOST_CONFIG_DIR") && before.contains("machine.token"),
                "{name} must consult leftover $FLEET_HOST_CONFIG_DIR/machine.token \
                 before the first-install throw"
            );
            let gate = &body[throw_at.saturating_sub(200)..throw_at];
            assert!(
                gate.contains("alreadyPaired") || gate.contains("already_paired"),
                "{name} first-install throw must be skipped when leftover machine.token \
                 is already paired"
            );
        }
    }

    /// Leftover whitespace-only machine.token is unpaired (MachineToken::parse)
    /// and must still fail closed before BIN_DIR.
    #[test]
    fn install_worker_windows_whitespace_machine_token_still_requires_pair_token() {
        for name in ["windows-x86_64.ps1", "windows-arm64.ps1"] {
            let body = script_body(name).expect("wired");
            let throw_at = windows_first_install_pair_token_throw_offset(body)
                .unwrap_or_else(|| panic!("{name} must throw on first-install missing pair token"));
            let mkdir_at = body
                .find("New-Item")
                .expect("platform script creates BIN_DIR");
            assert!(
                throw_at < mkdir_at,
                "{name} leftover whitespace machine.token must fail closed before BIN_DIR"
            );
            let before = &body[..throw_at];
            let token_file_at = before.find("machine.token").unwrap_or_else(|| {
                panic!("{name} must read leftover machine.token before the first-install throw")
            });
            assert!(
                before[token_file_at..].contains(".Trim()"),
                "{name} must Trim leftover machine.token so whitespace-only is unpaired"
            );
        }
    }

    struct LoopbackInstallProbe {
        success: bool,
        combined: String,
        requested_urls: String,
        wrote_bins: bool,
    }

    /// Run a served installer with a stub HTTP client.
    ///
    /// `health_body = Some(json)` is this-machine: `GET $FLEET_URL/health`
    /// identifies the control plane on this loopback. `None` is off-box:
    /// loopback is this process, not the plane.
    fn probe_loopback_install(
        script_name: &str,
        fleet_url: &str,
        health_body: Option<&str>,
    ) -> LoopbackInstallProbe {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let body =
            script_body(script_name).unwrap_or_else(|| panic!("{script_name} must be wired"));
        let script = tmp.path().join(script_name);
        fs::write(&script, body).unwrap();

        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let url_log = tmp.path().join("http.log");
        let stub = r#"#!/bin/sh
log=${FLEET_TEST_URL_LOG:?}
printf '%s\n' "$*" >> "$log"
url=""
for a in "$@"; do
  case $a in
    http://*|https://*) url=$a; printf '%s\n' "$a" >> "${log}.urls" ;;
  esac
done
case $url in
  */health|*/health/*)
    if [ -n "${FLEET_TEST_HEALTH_BODY:-}" ]; then
      printf '%s\n' "$FLEET_TEST_HEALTH_BODY"
      exit 0
    fi
    exit 22
    ;;
esac
exit 22
"#;
        for name in ["curl", "wget", "fetch"] {
            let path = bin.join(name);
            fs::write(&path, stub).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let prefix = tmp.path().join("prefix");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        let path_var = format!("{}:/usr/bin:/bin", bin.display());
        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        cmd.env_clear();
        cmd.env("PATH", &path_var);
        cmd.env("HOME", &home);
        cmd.env("LC_ALL", "C");
        cmd.env("FLEET_PREFIX", &prefix);
        cmd.env("FLEET_HOST_CONFIG_DIR", home.join("cfg"));
        cmd.env("FLEET_TEST_URL_LOG", &url_log);
        cmd.env("FLEET_URL", fleet_url);
        cmd.env("FLEET_INSTALL_ORIGIN", fleet_url);
        cmd.env("WORKER_PUBLIC_BASE", fleet_url);
        cmd.env("API_PUBLIC_BASE", "http://oauth.example.test:8080");
        cmd.env("FLEET_PAIR_TOKEN", "flp_testdeadbeef");
        if script_name != "install-worker.sh" {
            cmd.env("FLEET_PLATFORM", "test-platform");
        }
        if let Some(body) = health_body {
            cmd.env("FLEET_TEST_HEALTH_BODY", body);
        } else {
            cmd.env_remove("FLEET_TEST_HEALTH_BODY");
        }
        let output = cmd
            .output()
            .unwrap_or_else(|e| panic!("run served {script_name}: {e}"));
        let requested_urls =
            fs::read_to_string(format!("{}.urls", url_log.display())).unwrap_or_default();
        let dest_bin = prefix.join("bin");
        let wrote_bins = dest_bin.is_dir()
            && fs::read_dir(&dest_bin).ok().is_some_and(|rd| {
                rd.filter_map(|e| e.ok()).any(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.contains("fleet-host") || n.contains("fleet-driver")
                })
            });
        LoopbackInstallProbe {
            success: output.status.success(),
            combined: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            requested_urls,
            wrote_bins,
        }
    }

    fn fetched_install_bins(urls: &str) -> bool {
        urls.lines().any(|u| u.contains("/install/worker/bin/"))
    }

    fn fetched_only_health_probe(urls: &str) -> bool {
        let lines: Vec<&str> = urls.lines().filter(|l| !l.is_empty()).collect();
        !lines.is_empty() && lines.iter().all(|u| u.contains("/health"))
    }

    fn fetched_past_health(urls: &str) -> bool {
        urls.lines()
            .any(|u| !u.is_empty() && !u.contains("/health"))
    }

    const UNIX_LOOPBACK_SCRIPTS: &[&str] = &["install-worker.sh", "_unix_common.sh"];
    const LOOPBACK_FLEET_URLS: &[&str] = &[
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
        "http://127.1.2.3:8080",
    ];

    /// Off-box pair to 127/8, localhost, or [::1]: GET /health fails, so the
    /// served installer must exit non-zero before platform/bin fetch.
    #[test]
    fn install_loopback_url_warns_and_offbox_pair_fails_closed() {
        for script in UNIX_LOOPBACK_SCRIPTS {
            for url in LOOPBACK_FLEET_URLS {
                let off = probe_loopback_install(script, url, None);
                assert!(
                    !off.success,
                    "{script} must fail closed when {url} is loopback and this host \
                     is not the control plane:\nurls={}\nout={}",
                    off.requested_urls, off.combined
                );
                assert!(
                    !off.wrote_bins && !fetched_install_bins(&off.requested_urls),
                    "{script} must not fetch or write bins when pairing off-box to {url}:\n\
                     urls={}\nout={}",
                    off.requested_urls,
                    off.combined
                );
                assert!(
                    off.requested_urls.is_empty() || fetched_only_health_probe(&off.requested_urls),
                    "{script} off-box loopback gate must run before pair/bin fetch \
                     (health probe only):\nurls={}\nout={}",
                    off.requested_urls,
                    off.combined
                );
            }
        }
    }

    /// Same-box loopback: GET /health identifies the plane here, so install
    /// continues to the platform script or bins. The stub then dies (no bins).
    #[test]
    fn install_same_box_loopback_continues_after_health() {
        const FLEET_HEALTH: &str = r#"{"ok":true,"git_sha":"test"}"#;
        for script in UNIX_LOOPBACK_SCRIPTS {
            for url in LOOPBACK_FLEET_URLS {
                let same = probe_loopback_install(script, url, Some(FLEET_HEALTH));
                assert!(
                    same.requested_urls.lines().any(|u| u.contains("/health")),
                    "{script} same-box must probe {url}/health:\nurls={}\nout={}",
                    same.requested_urls,
                    same.combined
                );
                assert!(
                    fetched_past_health(&same.requested_urls),
                    "{script} must continue past the loopback gate when /health \
                     shows the plane is this machine ({url}):\nurls={}\nout={}",
                    same.requested_urls,
                    same.combined
                );
                assert!(
                    !same.success,
                    "{script} same-box continue must still die on the stub fetch \
                     ({url}):\nurls={}\nout={}",
                    same.requested_urls, same.combined
                );
                assert!(
                    !same.wrote_bins,
                    "{script} must not write fleet-host/fleet-driver from the stub \
                     ({url}):\nurls={}\nout={}",
                    same.requested_urls, same.combined
                );
            }
        }
    }

    /// LAN/public FLEET_URL must not take the loopback /health gate.
    #[test]
    fn install_lan_url_is_not_loopback() {
        let reachable = [
            "http://192.168.1.10:8080",
            "http://10.0.0.4:8080",
            "https://fleet.example.test",
        ];
        for script in UNIX_LOOPBACK_SCRIPTS {
            for url in reachable {
                let miss = probe_loopback_install(script, url, None);
                assert!(
                    fetched_past_health(&miss.requested_urls),
                    "{script} must not treat reachable {url} as loopback:\nurls={}\nout={}",
                    miss.requested_urls,
                    miss.combined
                );
                assert!(
                    !miss.requested_urls.lines().any(|u| u.contains("/health")),
                    "{script} must not probe /health for non-loopback {url}:\nurls={}\nout={}",
                    miss.requested_urls,
                    miss.combined
                );
                assert!(
                    !miss.success,
                    "{script} LAN continue must still die on the stub fetch ({url}):\n\
                     urls={}\nout={}",
                    miss.requested_urls, miss.combined
                );
                assert!(
                    !miss.wrote_bins,
                    "{script} must not write fleet-host/fleet-driver from the stub \
                     ({url}):\nurls={}\nout={}",
                    miss.requested_urls, miss.combined
                );
            }
        }
    }
}
