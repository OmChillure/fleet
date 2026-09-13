//! What this process should report as its installed software.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::LocalConfig;

/// Logical package names (no `.exe`). On-disk Windows names add the suffix.
pub const PACKAGE_BINS: [&str; 2] = ["fleet-host", "fleet-driver"];

/// Distinctive ASCII so `strings` / a sibling extract can read the version.
/// Prefixes are part of the contract — do not rename.
pub const HOST_PKG_STAMP_PREFIX: &[u8] = b"FLEET_HOST_PKG=";
pub const DRIVER_PKG_STAMP_PREFIX: &[u8] = b"FLEET_DRIVER_PKG=";

#[used]
static HOST_PKG_STAMP: &[u8] =
    concat!("FLEET_HOST_PKG=", env!("FLEET_HOST_PKG_VERSION"), "\0").as_bytes();

/// Compile-time tree-id (`FLEET_GIT_SHA`), same bake as fleet-server.
/// `build.rs` reruns rustc when the env changes so a cached object without
/// the sha cannot be reused. A 64-hex file hash is never a bake id.
pub fn baked_git_sha() -> Option<&'static str> {
    option_env!("FLEET_GIT_SHA").and_then(bake_id_from_raw)
}

fn bake_id_from_raw(raw: &str) -> Option<&str> {
    Some(raw.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| !is_file_sha256(s))
}

fn is_file_sha256(s: &str) -> bool {
    let s = s.trim();
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn host_pkg_version() -> &'static str {
    option_env!("FLEET_HOST_PKG_VERSION").unwrap_or("0.0.0")
}

/// Bake-time fleet-driver version (`fleet-host version` CLI). Heartbeat
/// does not use this — it reads the on-disk `FLEET_DRIVER_PKG=` stamp or omits.
pub fn driver_pkg_version() -> &'static str {
    option_env!("FLEET_DRIVER_PKG_VERSION").unwrap_or("0.0.0")
}

/// Version stamped into a binary (`FLEET_*_PKG=`). Same number on every OS.
pub fn pkg_version_from_bytes(bytes: &[u8], prefix: &[u8]) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let pos = bytes.windows(prefix.len()).position(|w| w == prefix)?;
    let rest = &bytes[pos + prefix.len()..];
    let end = rest
        .iter()
        .position(|&b| b == 0 || b.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let s = std::str::from_utf8(&rest[..end]).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn pkg_version_from_file(path: &Path, prefix: &[u8]) -> Option<String> {
    pkg_version_from_bytes(&fs::read(path).ok()?, prefix)
}

/// `updating` is true only while a **host-bin** apply is waiting / restarting.
/// `git_sha` is the compile-time bake id (or JSON null). File hashes stay
/// under `bins.*.sha256`.
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
        "fleet-host": {
            "sha256": host.as_deref().and_then(file_sha256),
            "version": host_pkg_version(),
        },
        "fleet-driver": {
            "sha256": resolve_bin(&cfg.fleet_driver_bin, host.as_deref())
                .as_deref()
                .and_then(file_sha256),
            "version": resolve_bin(&cfg.fleet_driver_bin, host.as_deref())
                .as_deref()
                .and_then(|p| pkg_version_from_file(p, DRIVER_PKG_STAMP_PREFIX)),
        },
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
    fn pkg_version_from_bytes_reads_stamp() {
        let blob = b"xxxxFLEET_DRIVER_PKG=9.9.9\0yyyy";
        assert_eq!(
            pkg_version_from_bytes(blob, DRIVER_PKG_STAMP_PREFIX).as_deref(),
            Some("9.9.9")
        );
    }

    #[test]
    fn pkg_stamp_prefixes_are_published_ascii() {
        assert_eq!(HOST_PKG_STAMP_PREFIX, b"FLEET_HOST_PKG=");
        assert_eq!(DRIVER_PKG_STAMP_PREFIX, b"FLEET_DRIVER_PKG=");
    }

    #[test]
    fn pkg_version_from_bytes_empty_value_is_none() {
        assert_eq!(
            pkg_version_from_bytes(b"FLEET_DRIVER_PKG=\0", DRIVER_PKG_STAMP_PREFIX),
            None
        );
    }

    #[test]
    fn pkg_version_from_bytes_stops_at_ascii_whitespace() {
        assert_eq!(
            pkg_version_from_bytes(b"FLEET_DRIVER_PKG=1.2.3\nrest", DRIVER_PKG_STAMP_PREFIX)
                .as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn bake_id_from_raw_drops_empty_whitespace_and_64_hex() {
        let file_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let tree_id = "0123456789abcdef0123456789abcdef01234567";
        let cases = [
            ("", None),
            ("  ", None),
            (file_hash, None),
            (tree_id, Some(tree_id)),
            ("abc123", Some("abc123")),
        ];
        for (raw, want) in cases {
            assert_eq!(bake_id_from_raw(raw), want, "bake_id_from_raw({raw:?})");
        }
    }

    #[test]
    fn heartbeat_software_updating_true_keeps_git_sha_and_bins() {
        let v = heartbeat_software(&LocalConfig::default(), true);
        assert_eq!(v.get("updating"), Some(&json!(true)));
        assert!(v.get("git_sha").is_some(), "git_sha key must be present");
        assert!(
            v["bins"]["fleet-host"]["version"].as_str().is_some(),
            "bins.fleet-host.version must stay set while updating: {v}"
        );
    }

    #[test]
    fn heartbeat_software_host_version_is_compile_time_not_on_disk_driver_stamp() {
        let dir = std::env::temp_dir().join(format!(
            "fleet-host-hb-hostver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let driver = dir.join("fleet-driver");
        fs::write(&driver, b"header\nFLEET_DRIVER_PKG=planted-drv-stamp\0tail").unwrap();
        let mut cfg = LocalConfig::default();
        cfg.fleet_driver_bin = driver.display().to_string();

        let v = heartbeat_software(&cfg, false);

        assert_eq!(
            v["bins"]["fleet-host"]["version"].as_str(),
            Some(host_pkg_version())
        );
        assert_ne!(
            v["bins"]["fleet-host"]["version"].as_str(),
            Some("planted-drv-stamp"),
            "host version must not come from the on-disk driver stamp: {v}"
        );

        let _ = fs::remove_file(&driver);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn heartbeat_driver_version_omitted_when_bin_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = LocalConfig::default();
        cfg.fleet_driver_bin = dir.path().join("missing-driver").display().to_string();

        let v = heartbeat_software(&cfg, false);

        assert!(
            v["bins"]["fleet-driver"]["version"].is_null(),
            "omit driver version when the file is missing: {v}"
        );
    }

    #[test]
    fn heartbeat_driver_version_omitted_when_unstamped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fleet-driver");
        fs::write(&p, b"no stamp in this blob").unwrap();
        let mut cfg = LocalConfig::default();
        cfg.fleet_driver_bin = p.display().to_string();

        let v = heartbeat_software(&cfg, false);

        assert!(
            v["bins"]["fleet-driver"]["version"].is_null(),
            "omit driver version when the on-disk stamp is missing: {v}"
        );
    }

    #[test]
    fn heartbeat_driver_version_only_from_on_disk_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fleet-driver");
        fs::write(&p, b"header\nFLEET_DRIVER_PKG=planted-not-bake\0tail").unwrap();
        let mut cfg = LocalConfig::default();
        cfg.fleet_driver_bin = p.display().to_string();

        let v = heartbeat_software(&cfg, false);

        assert_eq!(
            v["bins"]["fleet-driver"]["version"].as_str(),
            Some("planted-not-bake")
        );
        assert_ne!(
            v["bins"]["fleet-driver"]["version"].as_str(),
            Some(driver_pkg_version()),
            "must not invent the host bake version"
        );
    }

    #[test]
    fn resolve_bin_relative_name_uses_sibling_of_host_exe() {
        let dir = tempfile::tempdir().unwrap();
        let host_exe = dir.path().join("fake-host");
        let sibling = dir.path().join("fleet-driver");
        fs::write(&host_exe, b"host").unwrap();
        fs::write(&sibling, b"driver").unwrap();

        let got = resolve_bin("fleet-driver", Some(&host_exe));

        assert_eq!(got.as_deref(), Some(sibling.as_path()));
    }

    #[test]
    fn resolve_bin_absolute_existing_file_wins_over_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let host_exe = dir.path().join("fake-host");
        let sibling = dir.path().join("fleet-driver");
        let configured = dir.path().join("configured-driver");
        fs::write(&host_exe, b"host").unwrap();
        fs::write(&sibling, b"header\nFLEET_DRIVER_PKG=sibling-stamp\0tail").unwrap();
        fs::write(&configured, b"header\nFLEET_DRIVER_PKG=abs-stamp\0tail").unwrap();

        let got = resolve_bin(&configured.display().to_string(), Some(&host_exe));

        assert_eq!(got.as_deref(), Some(configured.as_path()));
        assert_ne!(got.as_deref(), Some(sibling.as_path()));
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
