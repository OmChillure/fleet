//! Platform IDs — same strings as `scripts/install-worker/install-worker.sh`.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::config::{ContainerBackend, LocalConfig};

/// Detect `linux-x86_64`, `darwin-arm64`, … Never panics.
pub fn detect_platform() -> String {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            #[cfg(windows)]
            {
                windows_platform_id()
            }
            #[cfg(not(windows))]
            {
                map_uname(&uname_s(), &uname_m(), is_musl())
            }
        })
        .clone()
}

#[cfg(windows)]
fn windows_platform_id() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "windows-x86_64".into(),
        "aarch64" => "windows-arm64".into(),
        other => format!("windows-{other}"),
    }
}

/// Console children (`hostname.exe`, Git `uname`, `wmic`, `powershell`)
/// allocate a visible window unless this flag is set. Heartbeat is 10s.
pub(crate) fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// Map uname `-s`/`-m` plus musl to the install-worker / staged-dist id.
fn map_uname(os: &str, arch: &str, musl: bool) -> String {
    let sys = match os {
        s if s.eq_ignore_ascii_case("linux") => "linux",
        s if s.eq_ignore_ascii_case("darwin") => "darwin",
        s if s.eq_ignore_ascii_case("freebsd") => "freebsd",
        s if s.eq_ignore_ascii_case("openbsd") => "openbsd",
        s if s.eq_ignore_ascii_case("netbsd") => "netbsd",
        s if looks_like_windows(&s) => "windows",
        s if s.eq_ignore_ascii_case("macos") => "darwin",
        other => other,
    };

    let cpu = match arch {
        "x86_64" | "amd64" | "x64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "armv7l" | "armv7" | "armhf" | "arm" => "armv7",
        "riscv64" => "riscv64",
        "ppc64le" | "powerpc64le" => "ppc64le",
        "s390x" => "s390x",
        other => other,
    };

    // WSL2 reuses Linux IDs (microsoft in uname -r).
    match sys {
        "linux" => {
            if musl {
                match cpu {
                    "x86_64" => "linux-x86_64-musl".into(),
                    "aarch64" => "linux-aarch64-musl".into(),
                    other => format!("linux-{other}-musl"),
                }
            } else {
                match cpu {
                    "x86_64" => "linux-x86_64".into(),
                    "aarch64" => "linux-aarch64".into(),
                    "armv7" => "linux-armv7".into(),
                    "riscv64" => "linux-riscv64".into(),
                    "ppc64le" => "linux-ppc64le".into(),
                    "s390x" => "linux-s390x".into(),
                    other => format!("linux-{other}"),
                }
            }
        }
        "darwin" => match cpu {
            "aarch64" => "darwin-arm64".into(),
            "x86_64" => "darwin-x86_64".into(),
            other => format!("darwin-{other}"),
        },
        "freebsd" => match cpu {
            "x86_64" => "freebsd-amd64".into(),
            "aarch64" => "freebsd-aarch64".into(),
            other => format!("freebsd-{other}"),
        },
        "openbsd" => format!("openbsd-{}", if cpu == "x86_64" { "amd64" } else { cpu }),
        "netbsd" => format!("netbsd-{}", if cpu == "x86_64" { "amd64" } else { cpu }),
        "windows" => match cpu {
            "x86_64" => "windows-x86_64".into(),
            "aarch64" => "windows-arm64".into(),
            other => format!("windows-{other}"),
        },
        other => format!("{other}-{cpu}"),
    }
}

fn looks_like_windows(s: &str) -> bool {
    let u = s.to_ascii_uppercase();
    u == "WINDOWS" || u.starts_with("MINGW") || u.starts_with("MSYS") || u.starts_with("CYGWIN")
}

#[cfg(not(windows))]
fn uname_s() -> String {
    if let Some(s) = run_uname(&["-s"]) {
        return s;
    }
    match std::env::consts::OS {
        "macos" => "Darwin".into(),
        other => other.to_string(),
    }
}

#[cfg(not(windows))]
fn uname_m() -> String {
    if let Some(s) = run_uname(&["-m"]) {
        return s;
    }
    std::env::consts::ARCH.to_string()
}

#[cfg(not(windows))]
fn run_uname(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("uname");
    hide_console(&mut cmd);
    let out = cmd.args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `COMPUTERNAME` / `HOSTNAME` first so Windows heartbeat does not spawn
/// `hostname.exe` (console subsystem) every 10s.
pub fn hostname() -> String {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(hostname_uncached).clone()
}

fn hostname_uncached() -> String {
    hostname_from_env(
        std::env::var("COMPUTERNAME").ok().as_deref(),
        std::env::var("HOSTNAME").ok().as_deref(),
    )
    .unwrap_or_else(hostname_from_process)
}

fn hostname_from_env(computername: Option<&str>, hostname_env: Option<&str>) -> Option<String> {
    for raw in [computername, hostname_env] {
        if let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
    }
    None
}

fn hostname_from_process() -> String {
    #[cfg(not(windows))]
    {
        if let Some(s) = run_uname(&["-n"]) {
            return s;
        }
        if let Ok(out) = {
            let mut cmd = Command::new("hostname");
            hide_console(&mut cmd);
            cmd.output()
        } {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    "unknown".into()
}

#[cfg(not(windows))]
fn is_musl() -> bool {
    if Path::new("/etc/alpine-release").is_file() {
        return true;
    }
    if let Ok(out) = Command::new("ldd").arg("/bin/sh").output() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.to_ascii_lowercase().contains("musl") {
            return true;
        }
    }
    cfg!(target_env = "musl")
}

pub fn command_exists(name: &str) -> bool {
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return true;
            }
            #[cfg(windows)]
            {
                let exe = dir.join(format!("{name}.exe"));
                if is_executable(&exe) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        return fs_mode(path).is_some_and(|m| m & 0o111 != 0);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn fs_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

pub fn advertised_host(cfg: &LocalConfig) -> bool {
    cfg.run_host
}

/// Token + `KUBERNETES_SERVICE_HOST` — the in-cluster kube client path.
pub fn in_cluster() -> bool {
    std::env::var_os("KUBERNETES_SERVICE_HOST").is_some()
        && Path::new("/var/run/secrets/kubernetes.io/serviceaccount/token").is_file()
}

/// `KUBECONFIG` paths, else `~/.kube/config`.
pub fn kubeconfig_present() -> bool {
    if let Ok(raw) = std::env::var("KUBECONFIG") {
        let split = if cfg!(windows) { ';' } else { ':' };
        if raw.split(split).any(|p| {
            let p = p.trim();
            !p.is_empty() && Path::new(p).is_file()
        }) {
            return true;
        }
    }
    crate::paths::home_dir()
        .join(".kube")
        .join("config")
        .is_file()
}

pub fn hypervisor_bins() -> &'static [&'static str] {
    &[
        "firecracker",
        "qemu-system-x86_64",
        "qemu-system-aarch64",
        "qemu-system-arm",
        "qemu",
    ]
}

pub fn hypervisor_on_path() -> bool {
    hypervisor_bins().iter().any(|b| command_exists(b))
}

/// Docker CLI or kube (in-cluster / kubeconfig) can run container isolation.
pub fn container_tools_available() -> bool {
    in_cluster() || command_exists("docker") || kubeconfig_present()
}

/// `run_container` and a container executor (docker or kube) is available.
pub fn advertised_container(cfg: &LocalConfig) -> bool {
    cfg.run_container && container_tools_available()
}

pub fn backend_ready(backend: ContainerBackend) -> bool {
    command_exists(backend.required_binary())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_uname_matches_install_worker_ids() {
        let cases = [
            ("Linux", "x86_64", false, "linux-x86_64"),
            ("linux", "amd64", true, "linux-x86_64-musl"),
            ("Darwin", "arm64", false, "darwin-arm64"),
            ("FreeBSD", "amd64", false, "freebsd-amd64"),
            ("MINGW64", "x86_64", false, "windows-x86_64"),
        ];
        for (os, arch, musl, want) in cases {
            assert_eq!(
                map_uname(os, arch, musl),
                want,
                "map_uname({os}, {arch}, {musl})"
            );
        }
    }

    #[test]
    fn map_uname_macos_and_aarch64_are_darwin_arm64() {
        assert_eq!(map_uname("macos", "arm64", false), "darwin-arm64");
        assert_eq!(map_uname("Darwin", "aarch64", false), "darwin-arm64");
    }

    #[test]
    fn hostname_is_non_empty_single_token() {
        let host = hostname();
        assert!(!host.is_empty(), "hostname must be non-empty");
        assert!(!host.contains('\n'), "hostname must be a single token");
    }

    #[test]
    fn hostname_from_env_prefers_computername_skips_blank() {
        assert_eq!(
            hostname_from_env(Some("X1"), Some("other")).as_deref(),
            Some("X1")
        );
        assert_eq!(
            hostname_from_env(Some("  "), Some("box")).as_deref(),
            Some("box")
        );
        assert_eq!(hostname_from_env(Some("  "), Some("\t")), None);
    }
}
