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
    Ok(script_response(&name, body))
}

fn lookup_worker_bin(state: &AppState, platform: &str, bin: &str) -> AppResult<PathBuf> {
    reject_unsafe_name(platform)?;
    reject_unsafe_name(bin)?;
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
    let path = lookup_worker_bin(&state, &platform, &bin)?;
    let ct = worker_bin_content_type(&bin);
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
fn native_platform_id() -> String {
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

/// Staged `manifest.json` for this platform, or `None` if it was not staged.
pub fn load_platform_manifest(
    state: &AppState,
    platform: Option<&str>,
) -> Option<WorkerSoftwareManifest> {
    let platform = platform.map(str::trim).filter(|s| !s.is_empty())?;
    if reject_unsafe_name(platform).is_err() {
        return None;
    }
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

fn script_response(name: &str, body: &'static str) -> Response {
    let ct = if name.ends_with(".sh") {
        HeaderValue::from_static("text/x-shellscript; charset=utf-8")
    } else {
        HeaderValue::from_static("text/plain; charset=utf-8")
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, ct)], body).into_response()
}

fn script_body(name: &str) -> Option<&'static str> {
    match name {
        "install-worker.sh" => Some(INSTALL_WORKER_SH),
        "install-worker.ps1" => Some(INSTALL_WORKER_PS1),
        "_unix_common.sh" => Some(UNIX_COMMON),
        "linux-x86_64.sh" => Some(LINUX_X86_64),
        "linux-aarch64.sh" => Some(LINUX_AARCH64),
        "linux-armv7.sh" => Some(LINUX_ARMV7),
        "linux-x86_64-musl.sh" => Some(LINUX_X86_64_MUSL),
        "linux-aarch64-musl.sh" => Some(LINUX_AARCH64_MUSL),
        "linux-armv7-musl.sh" => Some(LINUX_ARMV7_MUSL),
        "darwin-arm64.sh" => Some(DARWIN_ARM64),
        "darwin-x86_64.sh" => Some(DARWIN_X86_64),
        "freebsd-amd64.sh" => Some(FREEBSD_AMD64),
        "windows-x86_64.ps1" => Some(WINDOWS_X86_64),
        "windows-arm64.ps1" => Some(WINDOWS_ARM64),
        _ => None,
    }
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
    fn manifest_json_is_served_as_json() {
        assert_eq!(worker_bin_content_type("manifest.json"), "application/json");
        assert_eq!(
            worker_bin_content_type("fleet-host.sha256"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            worker_bin_content_type("fleet-host"),
            "application/octet-stream"
        );
    }

    #[test]
    fn map_uname_linux_x86_64() {
        assert_eq!(map_uname_platform("Linux", "x86_64", false), "linux-x86_64");
        assert_eq!(
            map_uname_platform("linux", "amd64", true),
            "linux-x86_64-musl"
        );
        assert_eq!(map_uname_platform("Linux", "armv7l", false), "linux-armv7");
        assert_eq!(
            map_uname_platform("Linux", "armv7l", true),
            "linux-armv7-musl"
        );
        assert_eq!(
            map_uname_platform("linux", "armhf", true),
            "linux-armv7-musl"
        );
        assert_eq!(map_uname_platform("Darwin", "arm64", false), "darwin-arm64");
        assert_eq!(
            map_uname_platform("FreeBSD", "amd64", false),
            "freebsd-amd64"
        );
    }

    #[test]
    fn script_body_serves_linux_armv7_musl() {
        let body = script_body("linux-armv7-musl.sh").expect("wired in script_body");
        assert!(
            body.contains("FLEET_PLATFORM=linux-armv7-musl"),
            "platform script must export linux-armv7-musl"
        );
        assert!(
            body.contains("fetch -o -"),
            "every Unix wrapper must accept fetch, not only freebsd-amd64"
        );
        assert!(
            body.contains("worker install: fetching installer"),
            "wrapper must print a phase line before the silent common fetch"
        );
    }

    #[test]
    fn script_body_names_host_and_driver() {
        let common = script_body("_unix_common.sh").expect("wired");
        assert!(
            common.contains("this machine is a self-hosted worker"),
            "installer must say the product is a worker"
        );
        assert!(
            common.contains("fleet-host   stays up"),
            "fleet-host is the machine daemon"
        );
        assert!(
            common.contains("fleet-driver runs each job"),
            "fleet-driver is the per-job process"
        );
        assert!(common.contains("worker install:"), "{common:.80}");
        assert!(
            !common.contains("fleet-host install:"),
            "error prefix is worker install:"
        );
        let boot = script_body("install-worker.sh").expect("wired");
        assert!(boot.contains("Install this worker from:"));
        assert!(!boot.contains("Install fleet-host from:"));
    }

    #[test]
    fn script_body_windows_arm64_fetches_libunwind() {
        let body = script_body("windows-arm64.ps1").expect("wired in script_body");
        assert!(
            body.contains("libunwind.dll"),
            "windows-arm64 installer must fetch gnullvm libunwind.dll"
        );
        assert!(
            body.contains("no runtime"),
            "libunwind 404 must be skip, not a hard fail"
        );
        assert!(
            body.contains(r#"GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")"#),
            "ARM64 installer must refuse x64-emulation arch"
        );
    }

    #[test]
    fn native_platform_id_is_a_single_token() {
        let id = native_platform_id();
        assert!(!id.is_empty(), "platform id must be non-empty");
        assert!(!id.contains('/'), "{id}");
        assert!(!id.contains('\n'), "{id}");
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
        write_bin(&exe, "linux-x86_64/fleet-host", b"staged");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(
            found.as_deref(),
            Some(exe.join("linux-x86_64/fleet-host").as_path())
        );

        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("exe");
        write_bin(&exe, "fleet-host", b"sibling");
        let found = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(found.as_deref(), Some(exe.join("fleet-host").as_path()));

        let found_foreign = resolve_worker_bin(
            None,
            Some(&exe),
            None,
            "darwin-arm64",
            "fleet-host",
            "linux-x86_64",
        );
        assert!(found_foreign.is_none());
    }

    #[test]
    fn resolve_workspace_target_release_then_debug_when_native() {
        let tmp = tempfile::tempdir().unwrap();
        write_bin(tmp.path(), "debug/fleet-host", b"debug");
        write_bin(tmp.path(), "release/fleet-host", b"release");
        let found = resolve_worker_bin(
            None,
            None,
            Some(tmp.path()),
            "linux-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert_eq!(
            found.as_deref(),
            Some(tmp.path().join("release/fleet-host").as_path())
        );

        let foreign = resolve_worker_bin(
            None,
            None,
            Some(tmp.path()),
            "windows-x86_64",
            "fleet-host",
            "linux-x86_64",
        );
        assert!(foreign.is_none());
    }
}
