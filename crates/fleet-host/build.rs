fn main() {
    // Heartbeat software.git_sha is option_env!("FLEET_GIT_SHA") — compile-time only.
    println!("cargo:rerun-if-env-changed=FLEET_GIT_SHA");

    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let packages = manifest.join("../../runner-packages.toml");
    println!("cargo:rerun-if-changed={}", packages.display());
    let text = std::fs::read_to_string(&packages).unwrap_or_default();
    println!(
        "cargo:rustc-env=FLEET_HOST_PKG_VERSION={}",
        section_value(&text, "versions", "fleet-host")
    );
    println!(
        "cargo:rustc-env=FLEET_DRIVER_PKG_VERSION={}",
        section_value(&text, "versions", "fleet-driver")
    );
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
    "0.0.0".into()
}
