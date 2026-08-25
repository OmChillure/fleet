//! What this process should report as its installed software.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::LocalConfig;

/// Logical package names (no `.exe`). On-disk Windows names add the suffix.
pub const PACKAGE_BINS: [&str; 2] = ["fleet-host", "fleet-driver"];

/// Compile-time tree-id (`FLEET_GIT_SHA`), same bake as fleet-server.
/// `build.rs` reruns rustc when the env changes so a cached object without
/// the sha cannot be reused.
pub fn baked_git_sha() -> Option<&'static str> {
    option_env!("FLEET_GIT_SHA").filter(|s| !s.is_empty())
}

/// `updating` is true only while a **host-bin** apply is waiting / restarting.
pub fn heartbeat_software(cfg: &LocalConfig, updating: bool) -> Value {
    json!({
        "git_sha": baked_git_sha(),
        "bins": bin_shas(cfg),
        "updating": updating,
    })
}

fn bin_shas(cfg: &LocalConfig) -> Value {
    let host = std::env::current_exe().ok();
    json!({
        "fleet-host": host.as_deref().and_then(file_sha256),
        "fleet-driver": resolve_bin(&cfg.fleet_driver_bin, host.as_deref())
            .as_deref()
            .and_then(file_sha256),
    })
}

pub(crate) fn resolve_bin(name: &str, host_exe: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }
    if let Some(dir) = host_exe.and_then(|h| h.parent()) {
        let sibling = dir.join(p.file_name().unwrap_or(p.as_os_str()));
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    which(name)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

pub(crate) fn file_sha256(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(sha256_hex(&bytes))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// On-disk file name for a package bin (`fleet-host.exe` on Windows).
pub fn package_bin_file_name(logical: &str, windows: bool) -> String {
    if windows && !logical.ends_with(".exe") {
        format!("{logical}.exe")
    } else {
        logical.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_software_always_sends_git_sha_key() {
        let v = heartbeat_software(&LocalConfig::default(), false);
        assert!(v.get("git_sha").is_some(), "git_sha key must be present");
        assert!(v.get("bins").is_some());
        assert_eq!(v.get("updating"), Some(&json!(false)));
        let updating = heartbeat_software(&LocalConfig::default(), true);
        assert_eq!(updating.get("updating"), Some(&json!(true)));
        match baked_git_sha() {
            Some(s) => {
                assert!(!s.is_empty());
                assert_eq!(v.get("git_sha"), Some(&json!(s)));
            }
            None => assert!(v.get("git_sha").unwrap().is_null()),
        }
    }

    #[test]
    fn file_sha256_hashes_bytes() {
        let dir = std::env::temp_dir().join(format!("fleet-host-sha-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let p = dir.join("x.bin");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            file_sha256(&p).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        let _ = fs::remove_file(&p);
    }
}
