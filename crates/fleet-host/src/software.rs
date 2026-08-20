//! What this process should report as its installed software.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::LocalConfig;

/// Compile-time tree-id (`FLEET_GIT_SHA`), same bake as fleet-server.
pub fn baked_git_sha() -> Option<&'static str> {
    option_env!("FLEET_GIT_SHA").filter(|s| !s.is_empty())
}

pub fn heartbeat_software(cfg: &LocalConfig) -> Value {
    json!({
        "git_sha": baked_git_sha(),
        "bins": bin_shas(cfg),
        "updating": false,
    })
}

fn bin_shas(cfg: &LocalConfig) -> Value {
    static CACHED: OnceLock<Value> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let host = std::env::current_exe().ok();
            json!({
                "fleet-host": host.as_deref().and_then(file_sha256),
                "fleet-worker": resolve_bin(&cfg.fleet_worker_bin, host.as_deref())
                    .as_deref()
                    .and_then(file_sha256),
                "fleet-driver": resolve_bin(&cfg.fleet_driver_bin, host.as_deref())
                    .as_deref()
                    .and_then(file_sha256),
            })
        })
        .clone()
}

fn resolve_bin(name: &str, host_exe: Option<&Path>) -> Option<PathBuf> {
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

fn file_sha256(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
