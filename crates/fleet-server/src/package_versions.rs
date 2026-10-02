//! Runner package versions (`runner-packages.toml`).
//!
//! `[versions]` is what staging writes into the bins. `[verify]` is what
//! this plane uses to decide current vs behind. `[release]` is the parent
//! hash of per-OS child hashes. A hash change without a new registered
//! version fails `scripts/check-worker-dist.sh` / deploy.

use std::cmp::Ordering;
use std::collections::BTreeMap;

const RAW: &str = include_str!("../../../runner-packages.toml");

/// Verification version — the number the update checker compares against.
pub fn fleet_host_version() -> &'static str {
    section_value("verify", "fleet-host")
}

/// Verification version — the number the update checker compares against.
pub fn fleet_driver_version() -> &'static str {
    section_value("verify", "fleet-driver")
}

/// Package version written into staged manifests / reported by the bins.
pub fn fleet_host_package_version() -> &'static str {
    section_value("versions", "fleet-host")
}

/// Package version written into staged manifests / reported by the bins.
pub fn fleet_driver_package_version() -> &'static str {
    section_value("versions", "fleet-driver")
}

fn section_value(section: &str, name: &str) -> &'static str {
    let header = format!("[{section}]");
    let needle = format!("{name} = \"");
    let mut in_section = false;
    for line in RAW.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix(&needle) {
            if let Some(end) = rest.find('"') {
                let v = &rest[..end];
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }
    "0.0.0"
}

/// Shipped OS packages locked in `[release.BIN]`. Not musl / armv7 / darwin.
pub const SHIP_RELEASE_PLATFORMS: &[&str] = &["linux-x86_64", "linux-aarch64", "windows-x86_64"];

/// `[release.<bin>].version` — independent of the other binary.
pub fn release_bin_version(bin: &str) -> &'static str {
    section_value(&format!("release.{bin}"), "version")
}

/// `[release.<bin>].parent` — sha256 of sorted `platform=hex\\n` children.
pub fn release_bin_parent(bin: &str) -> &'static str {
    section_value(&format!("release.{bin}"), "parent")
}

/// Per-OS hashes for one binary.
pub fn release_bin_children(bin: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for plat in SHIP_RELEASE_PLATFORMS {
        let sha = section_value(&format!("release.{bin}"), plat)
            .trim()
            .to_ascii_lowercase();
        if sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            out.insert((*plat).to_string(), sha);
        }
    }
    out
}

/// Parent digest of one binary's OS children. Same bytes as runner-release.py.
pub fn parent_digest(children: &BTreeMap<String, String>) -> String {
    let mut blob = String::new();
    for (plat, sha) in children {
        blob.push_str(plat);
        blob.push('=');
        blob.push_str(&sha.to_ascii_lowercase());
        blob.push('\n');
    }
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(blob.as_bytes()))
}

/// Plane `/health.git_sha` / compile-time `FLEET_GIT_SHA` (clean 40-hex or
/// dirty `HEAD-<12hex>`). Not a fleet-host / fleet-driver [verify] semver.
fn is_plane_bake_sha(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    if option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .is_some_and(|baked| !baked.is_empty() && baked == s)
    {
        return true;
    }
    let hex = s.split_once('-').map(|(h, _)| h).unwrap_or(s);
    hex.len() == 40 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// [verify] want, or none when the string is empty / a plane bake SHA.
fn verify_semver_want(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.is_empty() || is_plane_bake_sha(s) {
        None
    } else {
        Some(s)
    }
}

/// `true` when `package` is a higher semver than `verify`.
pub fn version_is_ahead(package: &str, verify: &str) -> bool {
    version_cmp(package, verify) == Ordering::Greater
}

pub fn version_cmp(a: &str, b: &str) -> Ordering {
    // A bake SHA is not a package version — do not order it against semver.
    if is_plane_bake_sha(a) || is_plane_bake_sha(b) {
        return Ordering::Equal;
    }
    let pa = semver_parts(a);
    let pb = semver_parts(b);
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

fn semver_parts(v: &str) -> Vec<u64> {
    v.split('.')
        .map(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

/// OS packages this plane is expected to serve. Keep in sync with
/// `scripts/stage-worker-dist.sh --all` and `scripts/check-worker-dist.sh`.
pub const REQUIRED_PACKAGE_PLATFORMS: &[&str] = &[
    "linux-x86_64",
    "linux-x86_64-musl",
    "linux-aarch64",
    "linux-aarch64-musl",
    "linux-armv7",
    "linux-armv7-musl",
    "windows-x86_64",
    "windows-arm64",
];

/// Staged fleet-host / fleet-driver versions for one OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedOsPackage<'a> {
    pub platform: &'a str,
    pub host: &'a str,
    pub driver: &'a str,
}

/// Required OS already present in `staged` whose host or driver is not current.
/// Never-staged required OS targets are omitted — they were never built.
pub fn stale_required_os<'a>(
    staged: &[StagedOsPackage<'a>],
    want_host: &str,
    want_driver: &str,
) -> Vec<&'static str> {
    let want_host = verify_semver_want(want_host);
    let want_driver = verify_semver_want(want_driver);
    if want_host.is_none() && want_driver.is_none() {
        return Vec::new();
    }
    staged
        .iter()
        .filter_map(|os| {
            let plat = os.platform.trim();
            let platform = REQUIRED_PACKAGE_PLATFORMS
                .iter()
                .copied()
                .find(|req| *req == plat)?;
            let host_ok = want_host.is_none_or(|w| os.host.trim() == w);
            let driver_ok = want_driver.is_none_or(|w| os.driver.trim() == w);
            if host_ok && driver_ok {
                return None;
            }
            Some(platform)
        })
        .collect()
}

/// Operator banner. `None` when every staged required OS is on both current versions.
pub fn stale_package_warning(
    staged: &[StagedOsPackage<'_>],
    want_host: &str,
    want_driver: &str,
) -> Option<String> {
    let stale = stale_required_os(staged, want_host, want_driver);
    if stale.is_empty() {
        return None;
    }
    let want_h = want_host.trim();
    let want_d = want_driver.trim();
    Some(format!(
        "OS builds not on fleet-host {want_h} / fleet-driver {want_d}: {}",
        stale.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_has_semver_for_both_bins() {
        for (name, v) in [
            ("[verify] fleet-host", fleet_host_version()),
            ("[verify] fleet-driver", fleet_driver_version()),
            ("[versions] fleet-host", fleet_host_package_version()),
            ("[versions] fleet-driver", fleet_driver_package_version()),
        ] {
            assert_ne!(
                v, "0.0.0",
                "{name} must not be the section_value missing-key fallback"
            );
            assert!(
                v.split('.').all(|p| p.chars().all(|c| c.is_ascii_digit())),
                "semver digits only ({name}): {v}"
            );
        }
    }

    #[test]
    fn verify_is_not_behind_package_versions() {
        assert!(
            !version_is_ahead(fleet_host_package_version(), fleet_host_version()),
            "fleet-host package {} is ahead of verify {} — bump [verify]",
            fleet_host_package_version(),
            fleet_host_version()
        );
        assert!(
            !version_is_ahead(fleet_driver_package_version(), fleet_driver_version()),
            "fleet-driver package {} is ahead of verify {} — bump [verify]",
            fleet_driver_package_version(),
            fleet_driver_version()
        );
    }

    #[test]
    fn version_is_ahead_compares_semver() {
        assert!(version_is_ahead("0.0.2", "0.0.1"));
        assert!(version_is_ahead("0.0.10", "0.0.9"));
        assert!(version_is_ahead("1.0.0", "0.9.9"));
        assert!(!version_is_ahead("0.0.2", "0.0.2"));
        assert!(!version_is_ahead("0.0.1", "0.0.2"));
        assert!(!version_is_ahead("", "0.0.1"));
        assert_eq!(version_cmp("0.0.2", "0.0.2"), Ordering::Equal);
        assert_eq!(version_cmp("0.0.1", "0.0.2"), Ordering::Less);
    }

    // Plane /health.git_sha and compile-time FLEET_GIT_SHA (clean 40-hex
    // or dirty HEAD-<12hex>). Not fleet-host / fleet-driver [verify].
    fn plane_bake_shas() -> Vec<&'static str> {
        let mut out = vec![
            "18a1be7df0a53136e7a85b741d8b9fc45b4cc351",
            "18a1be7df0a53136e7a85b741d8b9fc45b4cc351-e8260cd5c443",
            "abcdef0123456789abcdef0123456789abcdef01",
            "ffffffffffffffffffffffffffffffffffffffff",
        ];
        if let Some(sha) = option_env!("FLEET_GIT_SHA")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push(sha);
        }
        out
    }

    #[test]
    fn verify_versions_are_not_plane_bake_or_fleet_git_sha() {
        for (name, v) in [
            ("[verify] fleet-host", fleet_host_version()),
            ("[verify] fleet-driver", fleet_driver_version()),
            ("[versions] fleet-host", fleet_host_package_version()),
            ("[versions] fleet-driver", fleet_driver_package_version()),
        ] {
            assert!(
                v.split('.').all(|p| p.chars().all(|c| c.is_ascii_digit())),
                "{name} must be dotted-digit package semver, not plane identity: {v}"
            );
        }
    }

    #[test]
    fn version_helpers_do_not_treat_git_sha_as_verify_semver() {
        let verify = fleet_host_version();
        for sha in plane_bake_shas() {
            assert!(
                !version_is_ahead(sha, verify),
                "plane bake {sha} must not be ahead of [verify] {verify}"
            );
            assert!(
                !version_is_ahead(verify, sha),
                "[verify] {verify} must not treat bake {sha} as a behind package version"
            );
            assert!(
                !version_is_ahead(sha, sha),
                "a git sha is not a package version: {sha}"
            );
            assert_eq!(
                version_cmp(sha, verify),
                Ordering::Equal,
                "bake {sha} is not [verify] semver vs {verify}"
            );
            assert_eq!(
                version_cmp(verify, sha),
                Ordering::Equal,
                "[verify] {verify} is not ordered against bake {sha}"
            );
        }
        assert!(
            version_is_ahead("0.0.2", "0.0.1"),
            "real semver current/behind must still work"
        );
        assert!(!version_is_ahead("0.0.1", "0.0.2"));
    }

    #[test]
    fn release_table_is_self_consistent() {
        for (bin, pkg, verify) in [
            (
                "fleet-host",
                fleet_host_package_version(),
                fleet_host_version(),
            ),
            (
                "fleet-driver",
                fleet_driver_package_version(),
                fleet_driver_version(),
            ),
        ] {
            assert_eq!(
                release_bin_version(bin),
                pkg,
                "[release.{bin}].version must match [versions] {bin}"
            );
            assert_eq!(
                release_bin_version(bin),
                verify,
                "[release.{bin}].version must match [verify] {bin}"
            );
            let kids = release_bin_children(bin);
            assert_eq!(
                kids.len(),
                SHIP_RELEASE_PLATFORMS.len(),
                "{bin} must lock every shipped OS: {kids:?}"
            );
            assert_eq!(
                parent_digest(&kids),
                release_bin_parent(bin),
                "[release.{bin}].parent must be sha256 of sorted platform=hex children"
            );
        }
        assert_ne!(
            release_bin_parent("fleet-host"),
            release_bin_parent("fleet-driver"),
            "host and driver parents must be independent"
        );
    }

    #[test]
    fn parent_digest_changes_when_a_child_hash_changes() {
        let mut kids = release_bin_children("fleet-host");
        let first = kids.keys().next().cloned().expect("children");
        kids.insert(first, "ab".repeat(32));
        assert_ne!(
            parent_digest(&kids),
            parent_digest(&release_bin_children("fleet-host"))
        );
    }

    #[test]
    fn runner_release_py_accepts_table_when_dist_absent() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let py = root.join("scripts/runner-release.py");
        let status = std::process::Command::new("python3")
            .arg(&py)
            .arg("check")
            .arg("/no/such/fleet-worker-dist")
            .status()
            .expect("python3 runner-release.py");
        assert!(
            status.success(),
            "table-only check must pass: {py:?} {status}"
        );
    }
}
