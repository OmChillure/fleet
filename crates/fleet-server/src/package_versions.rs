//! Runner package versions (`runner-packages.toml`).
//!
//! `[versions]` is what staging writes into the bins. `[verify]` is what
//! this plane uses to decide current vs behind. File hashes still prove
//! the bytes on one OS.

use std::cmp::Ordering;

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

/// `true` when `package` is a higher semver than `verify`.
pub fn version_is_ahead(package: &str, verify: &str) -> bool {
    version_cmp(package, verify) == Ordering::Greater
}

pub fn version_cmp(a: &str, b: &str) -> Ordering {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_has_semver_for_both_bins() {
        assert!(
            fleet_host_version().chars().any(|c| c.is_ascii_digit()),
            "{}",
            fleet_host_version()
        );
        assert!(
            fleet_driver_version().chars().any(|c| c.is_ascii_digit()),
            "{}",
            fleet_driver_version()
        );
        assert_ne!(fleet_host_version(), "0.0.0");
        assert_ne!(fleet_driver_version(), "0.0.0");
        assert_ne!(fleet_host_package_version(), "0.0.0");
        assert_eq!(
            fleet_host_version(),
            fleet_host_version(),
            "same number for every OS of this crate"
        );
        assert!(
            fleet_host_version()
                .split('.')
                .all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "semver digits only: {}",
            fleet_host_version()
        );
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
}
