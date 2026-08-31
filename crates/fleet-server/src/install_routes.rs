//! Public install scripts and optional prebuilt worker binaries.

use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
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
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
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

    /// A planted control-plane or unknown name under dist is not a hit.
    #[test]
    fn resolve_rejects_control_plane_and_unknown_even_when_planted() {
        let tmp = tempfile::tempdir().unwrap();
        write_bin(tmp.path(), "linux-x86_64/fleet-server", b"not-a-worker");
        write_bin(tmp.path(), "linux-x86_64/.env", b"SECRET=1");
        write_bin(tmp.path(), "linux-x86_64/passwd", b"root:x:0:0");
        let host = write_bin(tmp.path(), "linux-x86_64/fleet-host", b"ok");
        assert!(resolve_worker_bin(
            Some(tmp.path()),
            None,
            None,
            "linux-x86_64",
            "fleet-server",
            "linux-x86_64",
        )
        .is_none());
        assert!(resolve_worker_bin(
            Some(tmp.path()),
            None,
            None,
            "linux-x86_64",
            ".env",
            "linux-x86_64",
        )
        .is_none());
        assert!(resolve_worker_bin(
            Some(tmp.path()),
            None,
            None,
            "linux-x86_64",
            "passwd",
            "linux-x86_64",
        )
        .is_none());
        assert_eq!(
            resolve_worker_bin(
                Some(tmp.path()),
                None,
                None,
                "linux-x86_64",
                "fleet-host",
                "linux-x86_64",
            )
            .as_deref(),
            Some(host.as_path())
        );
    }

    /// Unset dist and no exe/workspace fallbacks: no binary is found.
    #[test]
    fn worker_dist_unset_resolve_is_none() {
        let platform = "linux-x86_64";
        let bin = "fleet-host";
        assert_eq!(
            resolve_worker_bin(None, None, None, platform, bin, platform),
            None
        );
    }

    #[test]
    fn worker_dist_missing_bin_hint_includes_build_from_source() {
        let platform = "linux-x86_64";
        let bin = "fleet-host";
        let hint = missing_bin_hint(platform, bin);
        assert!(
            hint.contains("binary not available"),
            "hint must say the binary is missing: {hint}"
        );
        assert!(
            hint.contains("FLEET_WORKER_DIST_DIR"),
            "hint must name FLEET_WORKER_DIST_DIR: {hint}"
        );
        assert!(
            hint.contains("FLEET_WORKER_BUILD_FROM_SOURCE=1"),
            "hint must name FLEET_WORKER_BUILD_FROM_SOURCE=1: {hint}"
        );
        assert!(
            hint.contains(&format!("{platform}/{bin}")),
            "hint must name {platform}/{bin}: {hint}"
        );
    }

    /// Install scripts are wired even when FLEET_WORKER_DIST_DIR is unset.
    #[test]
    fn worker_dist_script_body_independent_of_dist() {
        assert!(
            script_body("install-worker.sh").is_some(),
            "install-worker.sh must stay served when dist is unset"
        );
        assert!(
            script_body("install-worker.ps1").is_some(),
            "install-worker.ps1 must stay served when dist is unset"
        );
    }

    struct PublicBaseProbe {
        success: bool,
        combined: String,
        requested_urls: String,
    }

    /// Run a served POSIX installer with both public bases in the child env.
    /// curl/wget/fetch are stubbed so a wrong origin is the URL log, not a
    /// real request (OAuth `API_PUBLIC_BASE` vs pair `WORKER_PUBLIC_BASE`).
    fn probe_script_public_bases(
        script_name: &str,
        fleet_url: Option<&str>,
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

    /// Pair URL is WORKER_PUBLIC_BASE via FLEET_URL; OAuth is API_PUBLIC_BASE.
    /// Mixing them locks out every runner or breaks Connect.
    #[test]
    fn install_scripts_use_worker_public_base_not_api_public_base() {
        let worker_public_base = "http://pair.example.test:9443";
        let api_public_base = "http://oauth.example.test:8080";
        assert_ne!(
            worker_public_base, api_public_base,
            "sentinels must differ so a collapsed base is visible"
        );

        for (name, body) in served_scripts() {
            assert!(
                !body.contains("API_PUBLIC_BASE"),
                "{name} must not reference API_PUBLIC_BASE (OAuth / public API)"
            );
            assert!(
                body.contains("FLEET_URL"),
                "{name} must take the pair URL via FLEET_URL (WORKER_PUBLIC_BASE)"
            );
        }

        let unix = script_body("install-worker.sh").expect("unix install command");
        let windows = script_body("install-worker.ps1").expect("windows install command");
        assert!(
            unix.contains("FLEET_URL="),
            "Unix install command must set FLEET_URL to the pair base"
        );
        assert!(
            !unix.contains("API_PUBLIC_BASE="),
            "Unix install command must not set API_PUBLIC_BASE"
        );
        assert!(
            windows.contains("$env:FLEET_URL"),
            "Windows install command must set FLEET_URL to the pair base"
        );
        assert!(
            !windows.contains("$env:API_PUBLIC_BASE"),
            "Windows install command must not set API_PUBLIC_BASE"
        );

        // install_hint sets FLEET_URL=<WORKER_PUBLIC_BASE>. A sourced .env may
        // still export API_PUBLIC_BASE — the child must fetch the worker origin,
        // not echo FLEET_URL and not pair against the OAuth base.
        for name in [
            "install-worker.sh",
            "_unix_common.sh",
            "linux-x86_64.sh",
            "darwin-arm64.sh",
            "freebsd-amd64.sh",
        ] {
            let probe = probe_script_public_bases(
                name,
                Some(worker_public_base),
                worker_public_base,
                api_public_base,
            );
            assert!(
                probe
                    .requested_urls
                    .lines()
                    .any(|u| u.starts_with(worker_public_base)),
                "{name} must fetch/pair against FLEET_URL=WORKER_PUBLIC_BASE={worker_public_base} \
                 (echoing FLEET_URL is not enough):\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
            assert!(
                !probe
                    .requested_urls
                    .lines()
                    .any(|u| u.starts_with(api_public_base)),
                "{name} fetched API_PUBLIC_BASE (OAuth) instead of WORKER_PUBLIC_BASE:\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
            assert!(
                !probe.combined.contains(api_public_base),
                "{name} must not treat sourced API_PUBLIC_BASE as the pair origin:\nurls={}\nout={}",
                probe.requested_urls,
                probe.combined
            );
        }
    }

    /// The UI one-liner sets `FLEET_URL`/`FLEET_PAIR_TOKEN` on the bootstrap
    /// `sh`. Those must be exported so `sh "$tmp"` and the piped
    /// `_unix_common.sh` still see the pair token and the worker (pair) base.
    #[test]
    fn install_worker_sh_exports_fleet_pair_token_and_fleet_url() {
        let boot = script_body("install-worker.sh").expect("unix install command");
        let export_token = boot
            .find("export FLEET_PAIR_TOKEN")
            .expect("install-worker.sh must export FLEET_PAIR_TOKEN for the platform child");
        let export_url = boot
            .find("export FLEET_URL")
            .expect("install-worker.sh must export FLEET_URL (WORKER_PUBLIC_BASE)");
        let child = boot
            .find("sh \"$tmp\"")
            .expect("bootstrap must run the downloaded platform script");
        assert!(
            export_token < child,
            "FLEET_PAIR_TOKEN must be exported before sh \"$tmp\""
        );
        assert!(
            export_url < child,
            "FLEET_URL must be exported before sh \"$tmp\""
        );
        assert!(
            !boot.contains("API_PUBLIC_BASE"),
            "install-worker.sh must not mention API_PUBLIC_BASE (OAuth / public API)"
        );
        assert!(
            !boot.contains("export API_PUBLIC_BASE"),
            "install-worker.sh must not export API_PUBLIC_BASE"
        );
    }

    /// Pair traffic is WORKER_PUBLIC_BASE via FLEET_URL. API_PUBLIC_BASE is
    /// OAuth / Connect — mixing them locks out every runner.
    #[test]
    fn install_worker_unix_hits_worker_public_base_not_api_public_base() {
        let worker_public_base = "http://pair.example.test:9443";
        let api_public_base = "http://oauth.example.test:8080";
        assert_ne!(worker_public_base, api_public_base);

        let common = script_body("_unix_common.sh").expect("unix common");
        assert!(
            common.contains("pair --url \"$ORIGIN\""),
            "unix common must pair against ORIGIN (FLEET_URL / WORKER_PUBLIC_BASE)"
        );
        assert!(
            common.contains("ORIGIN=\"${FLEET_INSTALL_ORIGIN:-${FLEET_URL:-}}\""),
            "unix common ORIGIN must come from FLEET_URL / FLEET_INSTALL_ORIGIN, not API_PUBLIC_BASE"
        );

        let win_x64 = script_body("windows-x86_64.ps1").expect("windows x64");
        let win_arm = script_body("windows-arm64.ps1").expect("windows arm64");
        for (name, body) in [
            ("windows-x86_64.ps1", win_x64),
            ("windows-arm64.ps1", win_arm),
        ] {
            assert!(
                body.contains("pair --url $Origin"),
                "{name} must pair against Origin (FLEET_URL / WORKER_PUBLIC_BASE)"
            );
            assert!(
                body.contains("$env:FLEET_URL"),
                "{name} Origin must come from FLEET_URL, not API_PUBLIC_BASE"
            );
        }

        let hit = probe_script_public_bases(
            "_unix_common.sh",
            Some(worker_public_base),
            worker_public_base,
            api_public_base,
        );
        assert!(
            hit.combined.contains(worker_public_base),
            "origin/pair URL must be FLEET_URL=WORKER_PUBLIC_BASE: {}",
            hit.combined
        );
        assert!(
            hit.requested_urls
                .lines()
                .any(|u| u.starts_with(worker_public_base)),
            "fetches must go to WORKER_PUBLIC_BASE, got: {}",
            hit.requested_urls
        );
        assert!(
            !hit.requested_urls.contains(api_public_base)
                && !hit.combined.contains(api_public_base),
            "must not fall through to API_PUBLIC_BASE:\nurls={}\nout={}",
            hit.requested_urls,
            hit.combined
        );

        // Sourced .env with only API_PUBLIC_BASE / WORKER_PUBLIC_BASE must not
        // become the pair origin — install_hint always sets FLEET_URL.
        let missing =
            probe_script_public_bases("_unix_common.sh", None, worker_public_base, api_public_base);
        assert!(
            !missing.success,
            "unset FLEET_URL must fail closed: {}",
            missing.combined
        );
        assert!(
            missing.combined.contains("FLEET_URL"),
            "fail-closed error must name FLEET_URL, got: {}",
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

    fn pair_token_required_check_offset(body: &str) -> Option<usize> {
        const MARKERS: &[&str] = &[
            "FLEET_PAIR_TOKEN is not set",
            "FLEET_PAIR_TOKEN is empty",
            "FLEET_PAIR_TOKEN required",
            "missing FLEET_PAIR_TOKEN",
            "need FLEET_PAIR_TOKEN",
            "[ -z \"${FLEET_PAIR_TOKEN",
            "[ -z \"$FLEET_PAIR_TOKEN",
            "[ -n \"${FLEET_PAIR_TOKEN:-}\" ] || die",
            "[ -n \"${FLEET_PAIR_TOKEN}\" ] || die",
            "[ -n \"$FLEET_PAIR_TOKEN\" ] || die",
        ];
        MARKERS.iter().filter_map(|m| body.find(m)).min()
    }

    /// Token check must be a hard require, and it must sit before mkdir / fetch.
    #[test]
    fn install_worker_unix_common_requires_pair_token_before_writing_bins() {
        let common = script_body("_unix_common.sh").expect("unix common");
        let token_at = pair_token_required_check_offset(common)
            .expect("_unix_common.sh must fail closed when FLEET_PAIR_TOKEN is missing or empty");
        let mkdir_at = common
            .find("mkdir -p \"$BIN_DIR\"")
            .expect("unix common creates BIN_DIR");
        let install_at = common
            .find("install_bins_from_cp")
            .expect("unix common writes bins via install_bins_from_cp");
        assert!(
            token_at < mkdir_at,
            "missing FLEET_PAIR_TOKEN must fail closed before mkdir BIN_DIR"
        );
        assert!(
            token_at < install_at,
            "missing FLEET_PAIR_TOKEN must fail closed before install_bins_from_cp"
        );
        assert!(
            !common.contains("optional FLEET_PAIR_TOKEN"),
            "FLEET_PAIR_TOKEN is required; treating it as optional writes bins then never pairs"
        );
    }

    struct UnixCommonRun {
        success: bool,
        combined: String,
        wrote_bins: bool,
    }

    /// Run the served `_unix_common.sh` in an isolated prefix. Platform is not
    /// `linux-*` so a missing token cannot `systemctl stop` a host unit.
    fn run_unix_common_install(pair_token: Option<&str>) -> UnixCommonRun {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("_unix_common.sh");
        fs::write(&script, script_body("_unix_common.sh").expect("wired")).unwrap();
        let prefix = tmp.path().join("prefix");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("HOME", &home);
        cmd.env("FLEET_PREFIX", &prefix);
        // Closed local port: connection refused if the script reaches fetch.
        cmd.env("FLEET_URL", "http://127.0.0.1:1");
        cmd.env("FLEET_PLATFORM", "test-platform");
        cmd.env("FLEET_HOST_CONFIG_DIR", home.join("cfg"));
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

    /// A real token must get past the require-token gate (and then fail on
    /// bins from the closed origin). Guards a script that always dies.
    #[test]
    fn install_worker_present_pair_token_passes_token_gate() {
        let run = run_unix_common_install(Some("flp_testdeadbeef"));
        assert!(
            !run.success,
            "closed origin must not install bins: {}",
            run.combined
        );
        assert!(
            !run.combined.contains("FLEET_PAIR_TOKEN is not set")
                && !run.combined.contains("FLEET_PAIR_TOKEN is empty")
                && !run.combined.contains("missing FLEET_PAIR_TOKEN")
                && !run.combined.contains("need FLEET_PAIR_TOKEN"),
            "set FLEET_PAIR_TOKEN must not fail the token gate: {}",
            run.combined
        );
        assert!(
            run.combined.contains("no test-platform bins")
                || run.combined.contains("need curl")
                || run.combined.contains("FLEET_URL"),
            "with a token, failure should be origin/bins, not the token: {}",
            run.combined
        );
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

    fn windows_pair_token_required_check_offset(body: &str) -> Option<usize> {
        const MARKERS: &[&str] = &[
            "FLEET_PAIR_TOKEN is not set",
            "FLEET_PAIR_TOKEN is empty",
            "FLEET_PAIR_TOKEN required",
            "missing FLEET_PAIR_TOKEN",
            "need FLEET_PAIR_TOKEN",
            "throw \"FLEET_PAIR_TOKEN",
            "throw \"set FLEET_PAIR_TOKEN",
            "throw 'FLEET_PAIR_TOKEN",
            "throw \"empty FLEET_PAIR_TOKEN",
        ];
        MARKERS.iter().filter_map(|m| body.find(m)).min()
    }

    fn windows_pair_token_is_trimmed(body: &str) -> bool {
        body.contains("FLEET_PAIR_TOKEN).Trim()")
            || body.contains("$env:FLEET_PAIR_TOKEN.Trim()")
            || body.contains("FLEET_PAIR_TOKEN').Trim()")
            || body.contains("FLEET_PAIR_TOKEN\".Trim()")
    }

    /// Catalog: Windows PowerShell ARM64 vs x64 from the *machine* (not this
    /// process). The served script must not fetch the other arch. Empty /
    /// whitespace FLEET_PAIR_TOKEN on a first install must fail closed
    /// (non-zero) before writing bins — same gate as `_unix_common.sh`.
    #[test]
    fn install_worker_ps1_selects_machine_arch_and_requires_pair_token() {
        let boot = script_body("install-worker.ps1").expect("windows bootstrap");
        let win_x64 = script_body("windows-x86_64.ps1").expect("windows x64");
        let win_arm = script_body("windows-arm64.ps1").expect("windows arm64");

        for (name, body) in [
            ("install-worker.ps1", boot),
            ("windows-x86_64.ps1", win_x64),
            ("windows-arm64.ps1", win_arm),
        ] {
            let detect = powershell_function_body(body, "Get-FleetPlatform");
            let machine = detect
                .find(r#"GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")"#)
                .unwrap_or_else(|| {
                    panic!(
                        "{name} Get-FleetPlatform must read Machine PROCESSOR_ARCHITECTURE \
                         (x64 PowerShell on ARM64 Windows reports AMD64)"
                    )
                });
            assert!(
                detect.contains(r#""ARM64" { return "windows-arm64" }"#),
                "{name} must map machine ARM64 to windows-arm64"
            );
            assert!(
                detect.contains(r#""AMD64" { return "windows-x86_64" }"#),
                "{name} must map machine AMD64 to windows-x86_64"
            );
            if let Some(process) = detect.find("$env:PROCESSOR_ARCHITECTURE") {
                assert!(
                    machine < process,
                    "{name} must prefer Machine PROCESSOR_ARCHITECTURE over this process"
                );
            }
        }

        assert!(
            boot.contains(r#"$origin/install/worker/${platform}.ps1"#)
                || boot.contains(r#"$origin/install/worker/$platform.ps1"#),
            "install-worker.ps1 must download the platform script for the detected machine"
        );
        assert!(
            !boot.contains("/install/worker/windows-x86_64.ps1")
                && !boot.contains("/install/worker/windows-arm64.ps1")
                && !boot.contains("/install/worker/bin/windows-x86_64")
                && !boot.contains("/install/worker/bin/windows-arm64"),
            "install-worker.ps1 must not hardcode an arch URL; a pinned path downloads the other PE"
        );
        assert!(
            boot.contains("FLEET_DETECT_ONLY") && boot.contains("Get-FleetPlatform"),
            "FLEET_DETECT_ONLY must print the machine platform and return before download"
        );

        for (name, body, this_plat, other_plat, want_pe) in [
            (
                "windows-x86_64.ps1",
                win_x64,
                "windows-x86_64",
                "windows-arm64",
                "amd64",
            ),
            (
                "windows-arm64.ps1",
                win_arm,
                "windows-arm64",
                "windows-x86_64",
                "arm64",
            ),
        ] {
            assert!(
                body.contains(&format!(r#"$ThisPlat = "{this_plat}""#)),
                "{name} must pin ThisPlat to {this_plat}"
            );
            assert!(
                body.contains(&format!(r#"/install/worker/bin/{this_plat}"#)),
                "{name} must fetch bins from /install/worker/bin/{this_plat}"
            );
            assert!(
                !body.contains(&format!("/install/worker/bin/{other_plat}")),
                "{name} must not download {other_plat} bins"
            );
            assert!(
                !body.contains(&format!("/install/worker/{other_plat}.ps1")),
                "{name} must not fetch the {other_plat} installer"
            );
            assert!(
                body.contains(&format!(r#"$WantPe = "{want_pe}""#)),
                "{name} must require {want_pe} PE, not the other arch"
            );
            let refuse = body
                .find("this machine is $detected, not $ThisPlat")
                .unwrap_or_else(|| {
                    panic!("{name} must refuse to run when the machine is not {this_plat}")
                });
            let save = body
                .find("function Save-Url")
                .or_else(|| body.find("Save-Url $url"))
                .expect("platform script downloads bins via Save-Url");
            assert!(
                refuse < save,
                "{name} must refuse the other arch before Save-Url"
            );
        }

        for (name, body) in [
            ("windows-x86_64.ps1", win_x64),
            ("windows-arm64.ps1", win_arm),
        ] {
            let token_at = windows_pair_token_required_check_offset(body).unwrap_or_else(|| {
                panic!(
                    "{name} must fail closed when FLEET_PAIR_TOKEN is missing or empty \
                     (throw / non-zero; do not print a 'next: pair' hint and exit 0)"
                )
            });
            let mkdir_at = body
                .find("New-Item -ItemType Directory -Force -Path $BinDir")
                .or_else(|| body.find("$BinDir = Join-Path $Prefix \"bin\""))
                .expect("platform script creates BIN_DIR");
            let download_at = body
                .find("Save-Url $url")
                .or_else(|| body.find("downloading $bin"))
                .expect("platform script downloads fleet-host / fleet-driver");
            assert!(
                token_at < mkdir_at,
                "{name} empty FLEET_PAIR_TOKEN must exit non-zero before creating BIN_DIR"
            );
            assert!(
                token_at < download_at,
                "{name} empty FLEET_PAIR_TOKEN must exit non-zero before downloading bins"
            );
            assert!(
                windows_pair_token_is_trimmed(body),
                "{name} must Trim() FLEET_PAIR_TOKEN so whitespace-only is empty"
            );
            assert!(
                !body.contains("optional FLEET_PAIR_TOKEN"),
                "{name} FLEET_PAIR_TOKEN is required on first install"
            );
        }
    }

    struct LoopbackInstallProbe {
        success: bool,
        combined: String,
        requested_urls: String,
        wrote_bins: bool,
    }

    fn has_loopback_offbox_warning(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        if !lower.contains("loopback") {
            return false;
        }
        lower.contains("another computer")
            || lower.contains("not this machine")
            || lower.contains("lan or public")
            || lower.contains("public control-plane")
            || lower.contains("public control plane")
            || lower.contains("reachable url")
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

    /// Loopback WORKER_PUBLIC_BASE / FLEET_URL: the served install command
    /// must warn. Pairing from an off-box host (loopback is this process,
    /// not the control plane) must fail closed before writing bins.
    /// Same-box loopback (GET /health identifies the plane here) may
    /// continue after the warning. LAN/public URLs must not take that gate.
    #[test]
    fn install_loopback_url_warns_and_offbox_pair_fails_closed() {
        const FLEET_HEALTH: &str = r#"{"ok":true,"git_sha":"test"}"#;
        let loopback_urls = [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
            "http://127.1.2.3:8080",
        ];
        let reachable = [
            "http://192.168.1.10:8080",
            "http://10.0.0.4:8080",
            "https://fleet.example.test",
        ];

        for name in [
            "install-worker.ps1",
            "windows-x86_64.ps1",
            "windows-arm64.ps1",
        ] {
            let body = script_body(name).unwrap_or_else(|| panic!("{name} must be wired"));
            let lower = body.to_ascii_lowercase();
            assert!(
                lower.contains("loopback"),
                "{name} must warn when FLEET_URL / WORKER_PUBLIC_BASE is loopback"
            );
            assert!(
                lower.contains("another computer")
                    || lower.contains("not this machine")
                    || lower.contains("lan or public")
                    || lower.contains("public control-plane")
                    || lower.contains("public control plane"),
                "{name} loopback warning must say another computer needs a LAN or public URL"
            );
        }

        for script in ["install-worker.sh", "_unix_common.sh"] {
            for url in loopback_urls {
                let off = probe_loopback_install(script, url, None);
                assert!(
                    has_loopback_offbox_warning(&off.combined),
                    "{script} must warn that {url} is loopback and another computer \
                     needs a LAN or public control-plane URL:\nurls={}\nout={}",
                    off.requested_urls,
                    off.combined
                );
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

                let same = probe_loopback_install(script, url, Some(FLEET_HEALTH));
                assert!(
                    has_loopback_offbox_warning(&same.combined),
                    "{script} must still warn when {url} is loopback on this machine:\n\
                     urls={}\nout={}",
                    same.requested_urls,
                    same.combined
                );
                let continued = same
                    .requested_urls
                    .lines()
                    .any(|u| !u.is_empty() && !u.contains("/health"));
                assert!(
                    continued,
                    "{script} must not fail-closed the loopback gate when /health \
                     shows the control plane is this machine ({url}); continue to \
                     fetch the platform script or bins:\nurls={}\nout={}",
                    same.requested_urls, same.combined
                );
            }

            for url in reachable {
                let miss = probe_loopback_install(script, url, None);
                assert!(
                    !has_loopback_offbox_warning(&miss.combined),
                    "{script} must not treat reachable {url} as loopback:\nurls={}\nout={}",
                    miss.requested_urls,
                    miss.combined
                );
                assert!(
                    !miss.combined.to_ascii_lowercase().contains("loopback"),
                    "{script} must not print a loopback warning for {url}:\nurls={}\nout={}",
                    miss.requested_urls,
                    miss.combined
                );
            }
        }
    }
}
