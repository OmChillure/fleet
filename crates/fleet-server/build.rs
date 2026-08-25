use std::cmp::Ordering;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // /health.git_sha is option_env!("FLEET_GIT_SHA") — compile-time only.
    // kubectl set env / runtime ENV must not be able to fake this tree.
    println!("cargo:rerun-if-env-changed=FLEET_GIT_SHA");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest_dir.join("../..");
    let toml_path = root.join("runner-packages.toml");
    println!("cargo:rerun-if-changed={}", toml_path.display());

    let text = fs::read_to_string(&toml_path).unwrap_or_default();
    let pkg_host = section_value(&text, "versions", "fleet-host");
    let pkg_driver = section_value(&text, "versions", "fleet-driver");
    let verify_host = section_value(&text, "verify", "fleet-host");
    let verify_driver = section_value(&text, "verify", "fleet-driver");

    if verify_host.is_empty() || verify_driver.is_empty() {
        println!(
            "cargo:warning=runner-packages.toml is missing [verify] fleet-host / fleet-driver — the update checker has nothing to compare against"
        );
    }
    warn_if_ahead("fleet-host", &pkg_host, &verify_host);
    warn_if_ahead("fleet-driver", &pkg_driver, &verify_driver);

    let dest = env::var("FLEET_WORKER_DIST_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".tmp/vms/dist"));
    warn_staged_ahead(&dest, &verify_host, &verify_driver);
}

fn warn_if_ahead(name: &str, package: &str, verify: &str) {
    if package.is_empty() || verify.is_empty() {
        return;
    }
    if version_is_ahead(package, verify) {
        println!(
            "cargo:warning={name} package version {package} is higher than verification version {verify} — bump [verify] in runner-packages.toml or the checker will flag every OS"
        );
    }
}

fn warn_staged_ahead(dest: &Path, verify_host: &str, verify_driver: &str) {
    let Ok(rd) = fs::read_dir(dest) else {
        return;
    };
    for ent in rd.flatten() {
        let manifest = ent.path().join("manifest.json");
        if !manifest.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", manifest.display());
        let Ok(raw) = fs::read_to_string(&manifest) else {
            continue;
        };
        let plat = ent.file_name().to_string_lossy().into_owned();
        let host = bin_version_from_manifest(&raw, "fleet-host");
        let driver = bin_version_from_manifest(&raw, "fleet-driver");
        if version_is_ahead(&host, verify_host) {
            println!(
                "cargo:warning={plat} fleet-host package {host} is higher than verification version {verify_host} — bump [verify] in runner-packages.toml"
            );
        }
        if version_is_ahead(&driver, verify_driver) {
            println!(
                "cargo:warning={plat} fleet-driver package {driver} is higher than verification version {verify_driver} — bump [verify] in runner-packages.toml"
            );
        }
    }
}

fn section_value(toml: &str, section: &str, name: &str) -> String {
    let header = format!("[{section}]");
    let prefix = format!("{name} = ");
    let mut in_section = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            let v = rest.trim().trim_matches('"').trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

fn bin_version_from_manifest(raw: &str, stem: &str) -> String {
    for key in [stem, &format!("{stem}.exe")] {
        let needle = format!("\"{key}\"");
        let Some(idx) = raw.find(&needle) else {
            continue;
        };
        let rest = &raw[idx + needle.len()..];
        let Some(vidx) = rest.find("\"version\"") else {
            continue;
        };
        let after = rest[vidx + "\"version\"".len()..].trim_start();
        let after = after.strip_prefix(':').unwrap_or(after).trim_start();
        if let Some(inner) = after.strip_prefix('"') {
            if let Some(end) = inner.find('"') {
                let v = inner[..end].trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    String::new()
}

fn version_is_ahead(package: &str, verify: &str) -> bool {
    if package.is_empty() || verify.is_empty() {
        return false;
    }
    version_cmp(package, verify) == Ordering::Greater
}

fn version_cmp(a: &str, b: &str) -> Ordering {
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
