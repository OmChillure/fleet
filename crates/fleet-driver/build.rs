fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let packages = manifest.join("../../runner-packages.toml");
    println!("cargo:rerun-if-changed={}", packages.display());
    let text = std::fs::read_to_string(&packages).unwrap_or_default();
    println!("cargo:rerun-if-env-changed=FLEET_DRIVER_PKG_VERSION");
    let ver = std::env::var("FLEET_DRIVER_PKG_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| section_value(&text, "versions", "fleet-driver"));
    println!("cargo:rustc-env=FLEET_DRIVER_PKG_VERSION={ver}");
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
