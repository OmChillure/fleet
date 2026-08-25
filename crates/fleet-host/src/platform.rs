//! Platform IDs — same strings as `scripts/install-worker/install-worker.sh`.

use std::path::Path;
use std::process::Command;

use crate::config::{ContainerBackend, LocalConfig};

/// Detect `linux-x86_64`, `darwin-arm64`, … Never panics.
pub fn detect_platform() -> String {
    let os = uname_s();
    let arch = uname_m();

    let sys = match os.as_str() {
        s if s.eq_ignore_ascii_case("linux") => "linux",
        s if s.eq_ignore_ascii_case("darwin") => "darwin",
        s if s.eq_ignore_ascii_case("freebsd") => "freebsd",
        s if s.eq_ignore_ascii_case("openbsd") => "openbsd",
        s if s.eq_ignore_ascii_case("netbsd") => "netbsd",
        s if looks_like_windows(&s) => "windows",
        s if s.eq_ignore_ascii_case("macos") => "darwin",
        other => other,
    };

    let cpu = match arch.as_str() {
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
            if is_musl() {
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

fn uname_s() -> String {
    if let Some(s) = run_uname(&["-s"]) {
        return s;
    }
    match std::env::consts::OS {
        "macos" => "Darwin".into(),
        other => other.to_string(),
    }
}

fn uname_m() -> String {
    if let Some(s) = run_uname(&["-m"]) {
        return s;
    }
    std::env::consts::ARCH.to_string()
}

fn run_uname(args: &[&str]) -> Option<String> {
    let out = Command::new("uname").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `uname -n`, then `$HOSTNAME`.
pub fn hostname() -> String {
    if let Some(s) = run_uname(&["-n"]) {
        return s;
    }
    if let Ok(out) = Command::new("hostname").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

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
    fn platform_detect_does_not_panic() {
        let id = detect_platform();
        assert!(!id.is_empty(), "platform id must be non-empty");
        assert!(!id.contains('\n'), "platform id must be a single token");
        // Known families from the install-worker detector.
        let ok = id.starts_with("linux-")
            || id.starts_with("darwin-")
            || id.starts_with("windows-")
            || id.starts_with("freebsd-")
            || id.starts_with("openbsd-")
            || id.starts_with("netbsd-");
        assert!(ok, "unexpected platform id {id}");
        let _ = hostname();
    }
}
