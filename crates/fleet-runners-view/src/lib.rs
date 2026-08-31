//! Host-testable self-hosted runner card formatters (extracted from fleet-ui).
//!
//! Pure predicates and display helpers. No Dioxus, no web-sys, no wasm.
//! Cloud / Partners placeholder copy lives in [`placeholders`].

pub mod placeholders;
pub mod routes;
mod types;

pub use types::{
    AgentEffort, AgentModelDetail, AgentRuntime, CreateHostResponse, HostDesiredSoftware, HostGpu,
    HostHardware, HostPatch, HostPublic, HostSoftware, InstallModelRequest, PackageInventory,
    PackageVersions,
};

/// Same phrase on Runners and Chat. `id` is `grok` / `claude` / `openai` / `ollama`.
pub fn agent_status_phrase(id: &str, installed: bool, logged_in: bool) -> &'static str {
    let uses_login = id != "ollama";
    if !installed {
        "not installed"
    } else if uses_login && !logged_in {
        "installed · logged out"
    } else if uses_login {
        "installed · logged in"
    } else {
        "installed"
    }
}

/// Official Claude Code `--model` aliases. Not pinned versions.
/// `claude models` is not a command — it starts an interactive session and hangs.
pub fn claude_cli_model_aliases() -> &'static [&'static str] {
    &["sonnet", "opus", "haiku", "fable"]
}

/// Official Claude Code `--effort` values (`claude --help`).
pub fn claude_effort_values() -> &'static [&'static str] {
    &["low", "medium", "high", "xhigh", "max"]
}

/// Elapsed send → reply for the Chat timer.
pub fn format_chat_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        let secs = ms as f64 / 1000.0;
        if secs < 10.0 {
            format!("{secs:.1}s")
        } else {
            format!("{}s", ms / 1000)
        }
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Catalog label for the four chat/runner providers.
/// Ollama is "Private (Ollama)" so it is not read as a cloud login.
pub fn catalog_provider_display_name(id: &str) -> &str {
    match id {
        "grok" => "Grok",
        "claude" => "Claude",
        "openai" => "OpenAI",
        "ollama" => "Private (Ollama)",
        other => other,
    }
}

/// One provider on a runner, used to pick Chat's default composer pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatProviderCandidate {
    pub id: String,
    pub available: bool,
    pub models: Vec<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatProviderPick {
    pub provider_id: String,
    pub model: String,
}

/// Prefer last-used when that provider is ready on this runner; otherwise
/// the first available (installed + ready) provider and its default model.
pub fn pick_chat_provider_and_model(
    providers: &[ChatProviderCandidate],
    preferred_provider: Option<&str>,
    preferred_model: Option<&str>,
) -> Option<ChatProviderPick> {
    if providers.is_empty() {
        return None;
    }
    let preferred_id = preferred_provider.map(str::trim).filter(|s| !s.is_empty());
    let preferred = preferred_id.and_then(|id| providers.iter().find(|p| p.id == id));

    let chosen = preferred
        .filter(|p| p.available)
        .or_else(|| providers.iter().find(|p| p.available))
        .or(preferred)
        .unwrap_or(&providers[0]);

    Some(ChatProviderPick {
        provider_id: chosen.id.clone(),
        model: resolve_chat_model(
            chosen,
            preferred.filter(|p| p.id == chosen.id).and(preferred_model),
        ),
    })
}

/// Snap the composer only when the user has not picked a pair and the
/// current one is empty, unknown, or not ready while another provider is.
pub fn should_apply_chat_defaults(
    providers: &[ChatProviderCandidate],
    current_provider: &str,
    user_picked: bool,
) -> bool {
    if user_picked || providers.is_empty() {
        return false;
    }
    if current_provider.trim().is_empty() {
        return true;
    }
    match providers.iter().find(|p| p.id == current_provider) {
        None => true,
        Some(p) => !p.available && providers.iter().any(|x| x.available),
    }
}

/// Match a model's effort menu. Empty model (CLI default) uses the first
/// advertised menu so the Effort picker still appears.
pub fn find_model_detail<'a>(
    details: &'a [crate::types::AgentModelDetail],
    model: &str,
) -> Option<&'a crate::types::AgentModelDetail> {
    let model = model.trim();
    if !model.is_empty() {
        if let Some(d) = details.iter().find(|d| d.name == model) {
            return Some(d);
        }
        if let Some(d) = details.iter().find(|d| {
            model.starts_with(&d.name)
                || d.name.starts_with(model)
                || d.name.starts_with(&format!("{model}:"))
        }) {
            return Some(d);
        }
    }
    details
        .iter()
        .find(|d| !d.efforts.is_empty())
        .or_else(|| details.first())
}

/// Last-used effort if this model still advertises it; otherwise the model's
/// own default, then the first listed value.
pub fn pick_effort_value(
    detail: Option<&crate::types::AgentModelDetail>,
    preferred: Option<&str>,
) -> Option<String> {
    let d = detail?;
    if d.efforts.is_empty() {
        return None;
    }
    if let Some(p) = preferred.map(str::trim).filter(|s| !s.is_empty()) {
        if d.efforts.iter().any(|e| e.value == p) {
            return Some(p.to_string());
        }
    }
    d.default_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| d.efforts.iter().any(|e| e.value == *s))
        .map(|s| s.to_string())
        .or_else(|| d.efforts.first().map(|e| e.value.clone()))
}

fn resolve_chat_model(provider: &ChatProviderCandidate, want: Option<&str>) -> String {
    let match_listed = |needle: &str| -> Option<String> {
        if provider.models.iter().any(|m| m == needle) {
            return Some(needle.to_string());
        }
        provider
            .models
            .iter()
            .find(|m| m.starts_with(&format!("{needle}:")) || m.starts_with(needle))
            .cloned()
    };
    if let Some(m) = want.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(hit) = match_listed(m) {
            return hit;
        }
    }
    if let Some(d) = provider
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(hit) = match_listed(d) {
            return hit;
        }
    }
    provider.models.first().cloned().unwrap_or_default()
}

pub fn status_label(status: &str) -> &str {
    match status {
        "disabled" => "removed",
        other => other,
    }
}

/// Heartbeats are ~10s. Stored `online` older than this is listed as `offline`.
/// 90s is several missed beats plus a roam, so a laptop does not flicker.
pub const HOST_OFFLINE_AFTER_MS: f64 = 90_000.0;

/// List-card status. Stored `online` with `last_seen_at` older than 90s is
/// `offline`. Pending / removed (`disabled`) / error keep their stored status —
/// the 90s rule must not overwrite them.
pub fn public_status(host: &HostPublic, now_ms: f64) -> &str {
    if host.status == "online" {
        match host
            .last_seen_at
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(parse_iso_millis)
        {
            Some(seen) if now_ms - seen <= HOST_OFFLINE_AFTER_MS => "online",
            _ => "offline",
        }
    } else {
        &host.status
    }
}

pub fn settings_awaiting_ack(host: &HostPublic) -> bool {
    host.settings_pending || host.settings_gen != host.settings_ack_gen
}

/// Extra chips next to online/offline: pending apply, outdated.
pub fn extra_status_pills(host: &HostPublic) -> Vec<&'static str> {
    let mut pills = Vec::new();
    if !is_control_plane(host)
        && settings_awaiting_ack(host)
        && host.status != "pending"
        && host.status != "disabled"
    {
        pills.push("pending apply");
    }
    if !is_control_plane(host) && software_flag(host) == Some("outdated") {
        pills.push("outdated");
    }
    pills
}

/// Status vs the current package versions. Never a git bake or a forced
/// "up to date" while the two binaries say something else.
pub fn software_flag(host: &HostPublic) -> Option<&'static str> {
    if host.software.as_ref().is_some_and(|s| s.updating) {
        return Some("updating");
    }
    let rows = software_bin_rows(host);
    if rows
        .iter()
        .any(|r| r.status == "behind" || r.status == "missing")
    {
        return Some("outdated");
    }
    if !rows.is_empty() && rows.iter().all(|r| r.status == "current") {
        return Some("up to date");
    }
    None
}

fn software_platform(host: &HostPublic) -> Option<&str> {
    host.platform
        .as_deref()
        .or_else(|| host.desired.as_ref().and_then(|d| d.platform.as_deref()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Compile-time tree-id baked into fleet-host. Internal only (plane-bake
/// skip). Not a runner version and not shown on the card.
///
/// A file hash (64-hex SHA-256) or a `git_sha` that is just a copy of a
/// reported bin SHA is not a bake id.
pub fn software_build_id(host: &HostPublic) -> Option<&str> {
    let sha = host
        .software
        .as_ref()
        .and_then(|s| s.git_sha.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if is_file_hash(sha) || reported_bin_sha_eq(host, sha) {
        return None;
    }
    Some(sha)
}

/// SHA-256 of a bin on disk — never a tree-id / bake stamp.
fn is_file_hash(s: &str) -> bool {
    let s = s.trim();
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn reported_bin_sha_eq(host: &HostPublic, sha: &str) -> bool {
    let Some(bins) = host.software.as_ref().and_then(|s| s.bins.as_ref()) else {
        return false;
    };
    let Some(map) = bins.as_object() else {
        return false;
    };
    map.values().any(|v| match v.as_str() {
        Some(got) => sha_eq(got, sha),
        None => v
            .get("sha256")
            .and_then(|x| x.as_str())
            .is_some_and(|got| sha_eq(got, sha)),
    })
}

/// What this plane will send a runner for one OS. `host`/`driver` are file hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedPlatform {
    pub platform: String,
    pub host: String,
    pub driver: String,
}

/// Platforms that have at least one of fleet-host / fleet-driver staged.
/// No git bake id — that stamp is not a runner version.
pub fn served_platforms(inv: &PackageInventory) -> Vec<ServedPlatform> {
    let mut out: Vec<ServedPlatform> = inv
        .packages
        .iter()
        .filter_map(|p| {
            let platform = p
                .platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())?
                .to_string();
            let host = p
                .bins
                .as_ref()
                .and_then(|b| lookup_bin_sha(b, "fleet-host"));
            let driver = p
                .bins
                .as_ref()
                .and_then(|b| lookup_bin_sha(b, "fleet-driver"));
            if host.is_none() && driver.is_none() {
                return None;
            }
            Some(ServedPlatform {
                platform,
                host: host.as_deref().map(short_sha).unwrap_or("—").to_string(),
                driver: driver.as_deref().map(short_sha).unwrap_or("—").to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.platform.cmp(&b.platform));
    out
}

fn short_sha(sha: &str) -> &str {
    let s = sha.trim();
    if s.len() > 12 { &s[..12] } else { s }
}

/// The two files the plane serves and the box must report. Not `fleet-worker`.
pub const PACKAGE_BIN_NAMES: [&str; 2] = ["fleet-host", "fleet-driver"];

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

/// One served OS whose staged bins are not the current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePackagePlatform {
    pub platform: &'static str,
    pub host: String,
    pub driver: String,
}

/// Served platforms (already in `inv.packages`) that are not the current version.
/// Never-staged OS targets are omitted — they are not stale, they were never built.
pub fn stale_package_platforms(inv: &PackageInventory) -> Vec<StalePackagePlatform> {
    let want_host = inv.package_versions.host.trim();
    let want_driver = inv.package_versions.driver.trim();
    if want_host.is_empty() && want_driver.is_empty() {
        return Vec::new();
    }
    inv.packages
        .iter()
        .filter_map(|p| {
            let plat = p
                .platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let platform = REQUIRED_PACKAGE_PLATFORMS
                .iter()
                .copied()
                .find(|req| *req == plat)?;
            let host = p
                .bins
                .as_ref()
                .and_then(|b| lookup_bin_version(b, "fleet-host"))
                .unwrap_or_default();
            let driver = p
                .bins
                .as_ref()
                .and_then(|b| lookup_bin_version(b, "fleet-driver"))
                .unwrap_or_default();
            let host_ok = !want_host.is_empty() && host == want_host;
            let driver_ok = !want_driver.is_empty() && driver == want_driver;
            if host_ok && driver_ok {
                return None;
            }
            Some(StalePackagePlatform {
                platform,
                host,
                driver,
            })
        })
        .collect()
}

/// Operator banner. `None` when every served OS is on the current versions.
pub fn stale_package_warning(inv: &PackageInventory) -> Option<String> {
    let stale = stale_package_platforms(inv);
    if stale.is_empty() {
        return None;
    }
    let want_h = inv.package_versions.host.trim();
    let want_d = inv.package_versions.driver.trim();
    let plats = stale
        .iter()
        .map(|s| s.platform)
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "OS builds not on fleet-host {want_h} / fleet-driver {want_d}: {plats}"
    ))
}

/// Self-hosted list header prefixes. Catalog / e2e: never "Current runner build".
pub const PACKAGE_HEADER_HOST_PREFIX: &str = "Current fleet-host version";
pub const PACKAGE_HEADER_DRIVER_PREFIX: &str = "Current fleet-driver version";

/// One header line per [verify] binary from `package_versions`.
/// Empty when that field is missing or is a bake SHA / file hash — those
/// are not runner versions. Never emits "Current runner build".
pub fn package_header_host_line(versions: &PackageVersions) -> Option<String> {
    package_header_line(PACKAGE_HEADER_HOST_PREFIX, &versions.host)
}

/// See [`package_header_host_line`].
pub fn package_header_driver_line(versions: &PackageVersions) -> Option<String> {
    package_header_line(PACKAGE_HEADER_DRIVER_PREFIX, &versions.driver)
}

/// Both header lines, host then driver. Skip a side that is not [verify] semver.
pub fn package_header_lines(versions: &PackageVersions) -> Vec<String> {
    [
        package_header_host_line(versions),
        package_header_driver_line(versions),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn package_header_line(prefix: &str, raw: &str) -> Option<String> {
    let version = verify_header_version(raw)?;
    Some(format!("{prefix} {version}"))
}

/// [verify] semver the header may show. Bake SHA / file hash / empty → none.
fn verify_header_version(raw: &str) -> Option<&str> {
    let s = raw.trim();
    if s.is_empty() || is_file_hash(s) || is_git_bake_id(s) || !looks_like_package_semver(s) {
        return None;
    }
    Some(s)
}

/// Plane `/health.git_sha` / compile-time `FLEET_GIT_SHA` (clean 40-hex or
/// dirty `HEAD-<12hex>`). Not a fleet-host / fleet-driver [verify] semver.
fn is_git_bake_id(s: &str) -> bool {
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
    hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn looks_like_package_semver(s: &str) -> bool {
    let core = s.split_once('-').map(|(c, _)| c).unwrap_or(s);
    let mut parts = core.split('.');
    let (Some(maj), Some(min), Some(pat), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [maj, min, pat]
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// One served binary vs what this box reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareBinRow {
    pub name: &'static str,
    /// Package semver the box reported, or em-dash. Never a file-hash prefix.
    pub version: String,
    pub status: &'static str,
}

pub fn software_bin_rows(host: &HostPublic) -> Vec<SoftwareBinRow> {
    PACKAGE_BIN_NAMES
        .iter()
        .map(|name| {
            let got_ver = reported_bin_version(host, name);
            let want_ver =
                desired_bin_version(host, name).or_else(|| current_pkg_version(host, name));
            let got = reported_bin_sha(host, name);
            let want = desired_bin_sha(host, name);
            let status = match (
                got_ver.as_deref(),
                want_ver.as_deref(),
                got.as_deref(),
                want.as_deref(),
            ) {
                (Some(g), Some(w), _, _) if sha_eq(g, w) => "current",
                (Some(_), Some(_), _, _) => "behind",
                (_, _, Some(g), Some(w)) if sha_eq(g, w) => "current",
                (_, _, Some(_), Some(_)) => "behind",
                (None, Some(_), None, _) | (_, _, None, Some(_)) => "missing",
                (Some(_), None, _, None) | (_, _, Some(_), None) => "reported",
                _ => "unknown",
            };
            let version = got_ver.or(want_ver).unwrap_or_else(|| "—".into());
            SoftwareBinRow {
                name,
                version,
                status,
            }
        })
        .collect()
}

fn sha_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn lookup_bin_sha(obj: &serde_json::Value, stem: &str) -> Option<String> {
    let map = obj.as_object()?;
    let exe = format!("{stem}.exe");
    let v = map.get(stem).or_else(|| map.get(exe.as_str()))?;
    if let Some(s) = v.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    let s = v.get("sha256")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn lookup_bin_version(obj: &serde_json::Value, stem: &str) -> Option<String> {
    let map = obj.as_object()?;
    let exe = format!("{stem}.exe");
    let v = map.get(stem).or_else(|| map.get(exe.as_str()))?;
    let s = v.get("version")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn reported_bin_version(host: &HostPublic, stem: &str) -> Option<String> {
    host.software
        .as_ref()
        .and_then(|s| s.bins.as_ref())
        .and_then(|b| lookup_bin_version(b, stem))
}

fn desired_bin_version(host: &HostPublic, stem: &str) -> Option<String> {
    host.desired
        .as_ref()
        .and_then(|d| d.bins.as_ref())
        .and_then(|b| lookup_bin_version(b, stem))
}

fn current_pkg_version(host: &HostPublic, stem: &str) -> Option<String> {
    let v = match stem {
        "fleet-host" => host.current_versions.host.as_str(),
        "fleet-driver" => host.current_versions.driver.as_str(),
        _ => return None,
    };
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn reported_bin_sha(host: &HostPublic, stem: &str) -> Option<String> {
    host.software
        .as_ref()
        .and_then(|s| s.bins.as_ref())
        .and_then(|b| lookup_bin_sha(b, stem))
}

fn desired_bin_sha(host: &HostPublic, stem: &str) -> Option<String> {
    host.desired
        .as_ref()
        .and_then(|d| d.bins.as_ref())
        .and_then(|b| lookup_bin_sha(b, stem))
}

pub fn software_line(host: &HostPublic) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = software_platform(host) {
        parts.push(p.to_string());
    }
    parts.push(software_flag(host).unwrap_or("unknown").to_string());
    parts.join(" · ")
}

pub fn software_title(host: &HostPublic) -> String {
    let plat = software_platform(host);
    match software_flag(host) {
        Some("up to date") => match plat {
            Some(p) => format!(
                "Both fleet-host and fleet-driver versions match the {p} package this plane is serving."
            ),
            None => {
                "Both fleet-host and fleet-driver versions match the package this plane is serving."
                    .into()
            }
        },
        Some("outdated") => {
            "fleet-host and/or fleet-driver versions do not match the package this plane is serving."
                .into()
        },
        Some("updating") => "Installing fleet-host and fleet-driver from this plane.".into(),
        Some("old package") => {
            "This box matches the files we still serve for this OS, but that package has no version. Restage this platform.".into()
        }
        _ => "This box has not reported fleet-host and fleet-driver versions we can compare.".into(),
    }
}

pub fn display_name(host: &HostPublic) -> String {
    let n = host.name.trim();
    let base = if n.is_empty() {
        "Unnamed worker".to_string()
    } else {
        n.to_string()
    };
    let mut tags: Vec<&str> = Vec::new();
    if host.is_control_plane_box {
        tags.push("control plane");
    }
    if host.is_this_machine {
        tags.push("this machine");
    }
    if tags.is_empty() {
        base
    } else {
        format!("{base} ({})", tags.join(" · "))
    }
}

pub fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let b = n as f64;
    if b < K {
        return format!("{n} B");
    }
    let kb = b / K;
    if kb < K {
        return format!("{kb:.1} KB");
    }
    let mb = kb / K;
    if mb < K {
        return format!("{mb:.1} MB");
    }
    let gb = mb / K;
    if gb < K {
        return format!("{gb:.1} GB");
    }
    format!("{:.1} TB", gb / K)
}

pub fn disk_free_line(host: &HostPublic) -> Option<String> {
    let hw = host.hardware.as_ref()?;
    let free = hw.disk_free_bytes?;
    match hw.disk_total_bytes {
        Some(total) if total > 0 => Some(format!(
            "{} free of {}",
            format_bytes(free),
            format_bytes(total)
        )),
        _ => Some(format!("{} free", format_bytes(free))),
    }
}

/// Official install one-liner for the runner's OS. Unix vs Windows only.
pub fn provider_install_command(provider: &str, platform: Option<&str>) -> String {
    let windows = platform
        .unwrap_or("")
        .to_ascii_lowercase()
        .starts_with("windows");
    match (provider, windows) {
        ("grok", false) => "curl -fsSL https://x.ai/cli/install.sh | bash".into(),
        ("grok", true) => "irm https://x.ai/cli/install.ps1 | iex".into(),
        ("claude", false) => "curl -fsSL https://claude.ai/install.sh | bash".into(),
        ("claude", true) => "irm https://claude.ai/install.ps1 | iex".into(),
        ("ollama", false) => "curl -fsSL https://ollama.com/install.sh | sh".into(),
        ("ollama", true) => "winget install Ollama.Ollama".into(),
        ("openai", false) => "npm install -g @openai/codex".into(),
        ("openai", true) => "npm install -g @openai/codex".into(),
        (other, _) => format!("Install {other} with the official CLI for this OS"),
    }
}

pub fn accepting_jobs(host: &HostPublic) -> bool {
    host.allow_host || host.allow_container
}

pub fn holding_new_jobs(host: &HostPublic) -> bool {
    host.paused || host.drain || !accepting_jobs(host)
}

pub fn hold_jobs_reason(host: &HostPublic) -> Option<String> {
    if !holding_new_jobs(host) {
        return None;
    }
    if host.current_jobs > 0 {
        Some(format!(
            "Not taking new jobs — {} already running will finish.",
            host.current_jobs
        ))
    } else {
        Some("Not taking new jobs. Running jobs were not stopped.".into())
    }
}

pub fn needs_software_update(host: &HostPublic) -> bool {
    host.outdated || host.software_status.as_deref() == Some("behind")
}

/// True after PATCH `request_update` until the host row is replaced.
pub fn update_requested(host: &HostPublic) -> bool {
    host.update_requested_at
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
}

/// One-shot Update now control. `auto_update` is the draft checkbox.
/// None on the plane, when auto is on, or when software is current.
pub fn update_now_label(host: &HostPublic, auto_update: bool) -> Option<&'static str> {
    if is_control_plane(host) {
        return None;
    }
    if update_requested(host) {
        return Some("Update requested");
    }
    // Auto-update on: the box pulls by itself. Off: operator uses Update now.
    if auto_update {
        return None;
    }
    Some("Update now")
}

pub fn accept_jobs_label(holding: bool) -> &'static str {
    if holding {
        "Take jobs again"
    } else {
        "Stop taking new jobs"
    }
}

pub fn accept_jobs_title(holding: bool) -> &'static str {
    if holding {
        "This worker will take new jobs again. Jobs already running were never stopped."
    } else {
        "Does not stop jobs already running here. New jobs go to another worker, or run on the control plane if none are free."
    }
}

pub fn not_taking_reason(host: &HostPublic) -> Option<String> {
    match host.status.as_str() {
        "disabled" => Some("Removed — will not take jobs.".into()),
        "pending" => Some("Waiting to pair.".into()),
        // Offline is already the status pill — do not repeat it as a sentence.
        "offline" => None,
        "error" => Some(
            host.last_error
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Error — not taking jobs.".into()),
        ),
        _ if holding_new_jobs(host) => hold_jobs_reason(host),
        "online" if !host.allow_host || !host.advertised_host => Some("Not taking jobs.".into()),
        _ => None,
    }
}

pub fn status_rank(status: &str) -> u8 {
    match status {
        "online" => 0,
        "pending" => 1,
        "error" => 2,
        "offline" => 3,
        "disabled" => 4,
        _ => 5,
    }
}

pub fn is_removed(host: &HostPublic) -> bool {
    host.status == "disabled"
}

pub fn is_control_plane(host: &HostPublic) -> bool {
    host.kind == "control_plane"
}

fn nonempty_opt(v: &Option<String>) -> bool {
    v.as_ref().is_some_and(|s| !s.trim().is_empty())
}

/// Paired at least once: token consumed or a heartbeat/identity reported.
fn ever_paired(host: &HostPublic) -> bool {
    nonempty_opt(&host.paired_at)
        || nonempty_opt(&host.last_seen_at)
        || nonempty_opt(&host.hostname)
        || nonempty_opt(&host.platform)
}

/// Show-removed list: disabled hosts that actually paired. Revoked pending
/// leftovers (never heartbeated) stay hidden. Older pairings of a live box
/// (same machine_id, or same hostname+platform) stay hidden too.
pub fn in_removed_list(host: &HostPublic, all: &[HostPublic]) -> bool {
    is_removed(host) && ever_paired(host) && !is_superseded_removed(host, all)
}

fn same_box(a: &HostPublic, b: &HostPublic) -> bool {
    if a.id == b.id {
        return false;
    }
    if let (Some(x), Some(y)) = (
        a.machine_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        b.machine_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        if x.eq_ignore_ascii_case(y) {
            return true;
        }
    }
    let ah = a.hostname.as_deref().map(str::trim).unwrap_or("");
    let bh = b.hostname.as_deref().map(str::trim).unwrap_or("");
    let ap = a.platform.as_deref().map(str::trim).unwrap_or("");
    let bp = b.platform.as_deref().map(str::trim).unwrap_or("");
    !ah.is_empty() && ah.eq_ignore_ascii_case(bh) && !ap.is_empty() && ap.eq_ignore_ascii_case(bp)
}

fn recency_key(host: &HostPublic) -> &str {
    host.last_seen_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            host.paired_at
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| host.created_at.as_deref().map(str::trim))
        .unwrap_or("")
}

fn is_superseded_removed(host: &HostPublic, all: &[HostPublic]) -> bool {
    if !is_removed(host) {
        return false;
    }
    all.iter().any(|o| {
        if o.id == host.id || !same_box(host, o) {
            return false;
        }
        if !is_removed(o) {
            return true;
        }
        let other = recency_key(o);
        let this = recency_key(host);
        other > this || (other == this && o.id.as_str() > host.id.as_str())
    })
}

pub fn worker_id_label(host: &HostPublic) -> String {
    let raw = host
        .machine_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(host.id.as_str());
    let hex: String = raw
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.len() >= 8 {
        hex[..8].to_string()
    } else {
        raw.chars().take(8).collect()
    }
}

/// Card `n / cap` and assignable skip. Plane ∩ advertised, each clamped
/// 1–256. Missing advertised uses the plane cap. Advertised `0` is 1, not unlimited.
pub fn job_cap(host: &HostPublic) -> i32 {
    let cp = host.max_concurrent.clamp(1, 256);
    match host.advertised_max_concurrent {
        Some(n) => cp.min(n.clamp(1, 256)),
        None => cp,
    }
}

/// True when `current_jobs` has reached [`job_cap`]. Same min as the card
/// `n / cap` and as assignable skip. Advertised `0` is a cap of 1, not unlimited.
pub fn at_job_cap(host: &HostPublic) -> bool {
    host.current_jobs.max(0) >= job_cap(host)
}

fn fmt_gib(bytes: u64) -> String {
    let g = bytes as f64 / 1_073_741_824.0;
    if g >= 10.0 {
        format!("{g:.0} GB")
    } else {
        format!("{g:.1} GB")
    }
}

fn fmt_mem(bytes: u64, speed_mts: Option<u32>, mem_type: Option<&str>) -> String {
    let size = fmt_gib(bytes);
    let kind = mem_type.map(str::trim).filter(|s| !s.is_empty());
    let speed = speed_mts.filter(|s| *s > 0);
    match (kind, speed) {
        (Some(t), Some(s)) => format!("{size} {t}-{s}"),
        (None, Some(s)) => format!("{size} {s} MT/s"),
        _ => size,
    }
}

fn clean_cpu_model(raw: &str) -> String {
    let mut s = raw.to_string();
    for tok in ["(R)", "(TM)", "(C)", "®", "™"] {
        s = s.replace(tok, "");
    }
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    for suffix in [
        " with Radeon Graphics",
        " with Radeon Vega Graphics",
        " with Mobile Radeon Graphics",
    ] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
        }
    }
    s
}

fn fmt_gpu(g: &HostGpu) -> Option<String> {
    let name = g.name.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    match g.vram_bytes.filter(|b| *b > 0) {
        Some(b) => Some(format!("{name} {}", fmt_gib(b))),
        None => Some(name.to_string()),
    }
}

pub fn specs_line(host: &HostPublic) -> Option<String> {
    let hw = host.hardware.as_ref()?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = hw
        .cpu_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(clean_cpu_model(m));
    }
    if let Some(n) = hw.cpu_cores.filter(|n| *n > 0) {
        parts.push(if n == 1 {
            "1 core".into()
        } else {
            format!("{n} cores")
        });
    }
    if let Some(b) = hw.mem_total_bytes.filter(|b| *b > 0) {
        parts.push(fmt_mem(b, hw.mem_speed_mts, hw.mem_type.as_deref()));
    }
    if let Some(gpus) = hw.gpus.as_ref() {
        let names: Vec<String> = gpus.iter().filter_map(fmt_gpu).collect();
        if !names.is_empty() {
            parts.push(names.join(", "));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Control plane first, then public-status rank (same as the pill), then name.
pub fn cmp_hosts_for_list(a: &HostPublic, b: &HostPublic, now_ms: f64) -> std::cmp::Ordering {
    is_control_plane(b)
        .cmp(&is_control_plane(a))
        .then_with(|| {
            status_rank(public_status(a, now_ms)).cmp(&status_rank(public_status(b, now_ms)))
        })
        .then_with(|| a.name.cmp(&b.name))
}

/// Sort in place. `now_ms` is milliseconds since Unix epoch (`Date.now()` in
/// the UI). Do not call `chrono::Utc::now()` here — that panics on wasm32
/// without chrono's `wasmbind` feature and blanks Self-hosted.
pub fn sort_hosts_for_list(hosts: &mut [HostPublic], now_ms: f64) {
    hosts.sort_by(|a, b| cmp_hosts_for_list(a, b, now_ms));
}

pub fn format_ts(ts: &str) -> String {
    if ts.is_empty() {
        return "—".into();
    }
    ts.get(..19).unwrap_or(ts).replace('T', " ")
}

fn parse_iso_millis(raw: &str) -> Option<f64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp_millis() as f64);
    }
    let compact = raw.get(..19)?;
    let naive =
        chrono::NaiveDateTime::parse_from_str(&compact.replace('T', " "), "%Y-%m-%d %H:%M:%S")
            .ok()?;
    Some(naive.and_utc().timestamp_millis() as f64)
}

/// Relative last-seen. `now_ms` is milliseconds since Unix epoch (`Date.now()`).
pub fn relative_seen(ts: &Option<String>, now_ms: f64) -> String {
    let Some(raw) = ts.as_ref().filter(|s| !s.is_empty()) else {
        return "never".into();
    };
    let Some(parsed) = parse_iso_millis(raw) else {
        return format_ts(raw);
    };
    let secs = ((now_ms - parsed) / 1000.0).max(0.0) as i64;
    if secs < 5 {
        "just now".into()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Last-seen chip: control plane is this process, not a heartbeat clock.
pub fn last_seen_label(host: &HostPublic, now_ms: f64) -> String {
    if is_control_plane(host) {
        "this process".into()
    } else {
        relative_seen(&host.last_seen_at, now_ms)
    }
}

/// Software line is paired boxes only — never the plane, pending pair, or removed.
pub fn shows_software_line(host: &HostPublic) -> bool {
    !is_control_plane(host) && host.status != "pending" && host.status != "disabled"
}

/// Auto-update checkbox is host cards only (including pending pair).
/// Catalog "paired only" means not the plane — this process does not pull a
/// host package — not "hide on pending". Removed cards hide Behavior behind
/// `if !revoked`.
pub fn shows_auto_update(host: &HostPublic) -> bool {
    !is_control_plane(host) && host.status != "disabled"
}

/// Jobs `n / cap` plus optional load % (cpu / mem / gpu).
pub fn jobs_line(host: &HostPublic) -> String {
    let mut parts = vec![format!("jobs {} / {}", host.current_jobs, job_cap(host))];
    if let Some(hw) = host.hardware.as_ref() {
        if let Some(pct) = hw.cpu_used_pct {
            parts.push(format!("cpu {pct}%"));
        }
        if let Some(pct) = hw.mem_used_pct {
            parts.push(format!("mem {pct}%"));
        }
        if let Some(pct) = hw.gpu_used_pct {
            parts.push(format!("gpu {pct}%"));
        }
    }
    parts.join(" · ")
}

/// Fail-closed clipboard gate. Empty never copies; the write is wasm-only.
pub fn clipboard_has_payload(text: &str) -> bool {
    !text.is_empty()
}

/// Hostname · platform on the card. Either side may be absent.
pub fn identity_line(host: &HostPublic) -> Option<String> {
    let hostname = host
        .hostname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let platform = host
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (hostname, platform) {
        (Some(h), Some(p)) => Some(format!("{h} · {p}")),
        (Some(h), None) => Some(h.to_string()),
        (None, Some(p)) => Some(p.to_string()),
        (None, None) => None,
    }
}

/// Draft Behavior widgets differ from the last applied host.
pub fn settings_draft_dirty(
    host: &HostPublic,
    accepting: bool,
    auto_update: bool,
    max_concurrent: i32,
) -> bool {
    accepting != accepting_jobs(host)
        || (!is_control_plane(host) && auto_update != host.auto_update)
        || max_concurrent != host.max_concurrent
}

/// Isolation + auto-update + cap lock while a paired box has unacked settings.
pub fn widgets_locked(host: &HostPublic, busy: bool) -> bool {
    busy || (!is_control_plane(host) && settings_awaiting_ack(host))
}

/// Draft hold is the same as `holding_new_jobs` (not taking work).
pub fn draft_holding_new_jobs(accepting: bool) -> bool {
    !accepting
}

/// How this runner executes. Plane shows workspace SANDBOX_BACKEND; a paired host is process.
pub fn runtime_line(host: &HostPublic, sandbox_backend: &str) -> String {
    if is_control_plane(host) {
        let backend = sandbox_backend.trim();
        let backend = if backend.is_empty() { "stub" } else { backend };
        format!("Runtime: {backend} (SANDBOX_BACKEND)")
    } else {
        "Runtime: process on this machine".into()
    }
}

/// Max jobs at once (1–256). Same clamp as `job_cap` on the plane side.
pub fn clamp_max_jobs(n: i32) -> i32 {
    n.clamp(1, 256)
}

/// Card section above hold / auto-update / cap.
pub const BEHAVIOR_LABEL: &str = "Behavior";

pub fn hint_is_loopback(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

/// Install + pair command from `install_hint`, or a fallback using `api_base`.
pub fn install_commands(created: &CreateHostResponse, api_base: &str) -> String {
    let hint = created.install_hint.trim();
    if !hint.is_empty() {
        return hint.to_string();
    }
    format!(
        "curl -fsSL {api_base}/install-worker.sh | FLEET_URL={api_base} FLEET_PAIR_TOKEN={} sh",
        created.pair_token
    )
}

/// Control-plane URL the install command will call (from hint, else `api_base`).
pub fn install_api_url(created: &CreateHostResponse, api_base: &str) -> String {
    let hint = install_commands(created, api_base);
    if let Some(rest) = hint.split("FLEET_URL=").nth(1) {
        let url = rest
            .split(|c: char| c.is_whitespace() || c == '|')
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '\'' || c == '"');
        if url.starts_with("http") {
            return url.to_string();
        }
    }
    api_base.to_string()
}

/// PowerShell one-liner: download, pair, start (Windows).
pub fn windows_install_command(created: &CreateHostResponse, api_base: &str) -> String {
    let api = install_api_url(created, api_base);
    format!(
        "$env:FLEET_URL='{api}'; $env:FLEET_PAIR_TOKEN='{}'; irm \"$env:FLEET_URL/install-worker.ps1\" | iex",
        created.pair_token
    )
}

pub fn apply_patch(mut host: HostPublic, patch: &HostPatch) -> HostPublic {
    let mut bump = false;
    if let Some(v) = patch.name.clone() {
        if v != host.name {
            bump = true;
        }
        host.name = v;
    }
    if let Some(v) = patch.paused {
        if v != host.paused {
            bump = true;
        }
        host.paused = v;
    }
    if let Some(v) = patch.drain {
        if v != host.drain {
            bump = true;
        }
        host.drain = v;
    }
    if let Some(v) = patch.allow_host {
        if v != host.allow_host {
            bump = true;
        }
        host.allow_host = v;
    }
    if let Some(v) = patch.allow_container {
        if v != host.allow_container {
            bump = true;
        }
        host.allow_container = v;
    }
    if let Some(v) = patch.isolation.clone() {
        host.isolation = Some(v);
        bump = true;
    }
    if let Some(v) = patch.max_job_timeout_s {
        host.max_job_timeout_s = v;
        bump = true;
    }
    if let Some(v) = patch.runtime_settings.clone() {
        host.runtime_settings = Some(v);
        bump = true;
    }
    if let Some(v) = patch.max_concurrent {
        if v != host.max_concurrent {
            bump = true;
        }
        host.max_concurrent = v;
    }
    if let Some(v) = patch.auto_update {
        if v != host.auto_update {
            bump = true;
        }
        host.auto_update = v;
    }
    if patch.request_update == Some(true) {
        host.update_requested_at = Some("requested".into());
    }
    // Paired hosts wait for fleet-host to ack the new gen. The plane is this
    // process: apply is immediate (no pending apply, no gen/ack cycle).
    if bump && !is_control_plane(&host) {
        host.settings_gen = host.settings_gen.saturating_add(1);
        host.settings_pending = true;
    }
    host
}

#[cfg(test)]
mod tests {
    // catalog:unit-fleet-ui-helpers — host-runnable runner card formatters
    // extracted from fleet-ui (software line, pills, sort, pair install).
    use super::*;

    fn sample_host() -> HostPublic {
        serde_json::from_str(r#"{"id":"h1","workspace_id":"w1","name":"box","status":"online"}"#)
            .expect("sample host")
    }

    /// Reported vs served package versions. Extra "outdated" is this compare,
    /// not the API `outdated` / `software_status` fields.
    fn set_package_versions(host: &mut HostPublic, reported: &str, served: &str) {
        host.current_versions = PackageVersions {
            host: served.into(),
            driver: served.into(),
        };
        host.desired = Some(HostDesiredSoftware {
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": "served-host", "version": served},
                "fleet-driver": {"sha256": "served-drv", "version": served}
            })),
            ..HostDesiredSoftware::default()
        });
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host":{{"sha256":"got-host","version":"{reported}"}},"fleet-driver":{{"sha256":"got-drv","version":"{reported}"}}}}}}"#
        ))
        .expect("software");
    }

    /// Card renders `{row.name} {row.version} · {row.status}` from
    /// `software_bin_rows`. Version is package semver or an em-dash —
    /// never a file-hash prefix (`software_line` is not shown).
    fn is_package_semver_or_emdash(s: &str) -> bool {
        if s == "—" {
            return true;
        }
        let core = s.split_once('-').map(|(c, _)| c).unwrap_or(s);
        let mut parts = core.split('.');
        let (Some(maj), Some(min), Some(pat), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        [maj, min, pat]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }

    fn is_hex_prefix(s: &str) -> bool {
        let s = s.trim();
        (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn is_sha256_hex(s: &str) -> bool {
        let s = s.trim();
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn reported_bin_shas(host: &HostPublic) -> Vec<String> {
        let Some(bins) = host.software.as_ref().and_then(|s| s.bins.as_ref()) else {
            return Vec::new();
        };
        let Some(map) = bins.as_object() else {
            return Vec::new();
        };
        map.values()
            .filter_map(|v| {
                if let Some(s) = v.as_str() {
                    let s = s.trim();
                    return (!s.is_empty()).then(|| s.to_string());
                }
                v.get("sha256")
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect()
    }

    fn assert_card_row_versions_are_semver_or_emdash(host: &HostPublic) {
        let shas = reported_bin_shas(host);
        for row in software_bin_rows(host) {
            assert!(
                is_package_semver_or_emdash(&row.version),
                "card renders `{} {} · {}`; version must be package semver or em-dash, never a hash prefix",
                row.name,
                row.version,
                row.status
            );
            assert!(
                !is_hex_prefix(&row.version),
                "card row `{}` version {:?} is a hash prefix",
                row.name,
                row.version
            );
            for sha in &shas {
                let prefix = if sha.len() >= 12 {
                    &sha[..12]
                } else {
                    sha.as_str()
                };
                assert_ne!(
                    row.version.as_str(),
                    prefix,
                    "{} version must not be the first 12 of bin SHA {sha}",
                    row.name
                );
                assert!(
                    !row.version.contains(sha.as_str()),
                    "{} version must not contain bin SHA {sha}",
                    row.name
                );
            }
        }
    }

    fn assert_software_build_id_never_hash_or_bin_sha(host: &HostPublic) {
        if let Some(id) = software_build_id(host) {
            assert!(
                !is_sha256_hex(id),
                "software_build_id must never be a 64-hex file hash: {id}"
            );
            for sha in reported_bin_shas(host) {
                assert_ne!(
                    id,
                    sha.as_str(),
                    "software_build_id must not be a reported bin SHA"
                );
            }
        }
    }

    fn assert_no_pause(text: &str) {
        let lower = text.to_ascii_lowercase();
        assert!(
            !lower.contains("pause") && !lower.contains("resume"),
            "hold copy must not say Pause/Resume: {text}"
        );
    }

    /// Same strings the list row + detail panel render from this crate.
    fn card_copy(host: &HostPublic, now_ms: f64) -> String {
        let mut parts = vec![
            status_label(public_status(host, now_ms)).to_string(),
            last_seen_label(host, now_ms),
        ];
        parts.extend(extra_status_pills(host).iter().copied().map(str::to_string));
        if let Some(why) = not_taking_reason(host) {
            parts.push(why);
        }
        parts.join(" ")
    }

    fn assert_no_heartbeating(text: &str) {
        assert!(
            !text.to_ascii_lowercase().contains("heartbeat"),
            "offline card must not say heartbeating: {text}"
        );
    }

    #[test]
    fn accept_jobs_copy_has_no_pause() {
        for holding in [false, true] {
            assert_no_pause(accept_jobs_label(holding));
            assert_no_pause(accept_jobs_title(holding));
        }
        assert_eq!(accept_jobs_label(false), "Stop taking new jobs");
        assert_eq!(accept_jobs_label(true), "Take jobs again");
        assert!(accept_jobs_title(false).contains("Does not stop jobs already running"));
        assert!(accept_jobs_title(true).contains("were never stopped"));
    }

    #[test]
    fn specs_line_is_iron_plus_load_ready() {
        let mut host = sample_host();
        host.hardware = Some(HostHardware {
            cpu_model: Some("AMD Ryzen 7 5700G with Radeon Graphics".into()),
            cpu_cores: Some(16),
            mem_total_bytes: Some(32 * 1_073_741_824),
            mem_speed_mts: Some(3200),
            mem_type: Some("DDR4".into()),
            gpus: Some(vec![HostGpu {
                name: Some("Radeon Vega 8".into()),
                vram_bytes: None,
                used_pct: None,
            }]),
            os: Some("linux 6.8 x86_64".into()),
            mem_used_pct: Some(32),
            cpu_used_pct: Some(12),
            gpu_used_pct: Some(8),
            ..HostHardware::default()
        });
        assert_eq!(
            specs_line(&host).as_deref(),
            Some("AMD Ryzen 7 5700G · 16 cores · 32 GB DDR4-3200 · Radeon Vega 8")
        );
    }

    #[test]
    fn specs_line_shows_intel_uhd() {
        let mut host = sample_host();
        host.hardware = Some(HostHardware {
            cpu_model: Some("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz".into()),
            cpu_cores: Some(8),
            gpus: Some(vec![HostGpu {
                name: Some("Intel UHD Graphics".into()),
                vram_bytes: None,
                used_pct: Some(4),
            }]),
            ..HostHardware::default()
        });
        let line = specs_line(&host).expect("specs");
        assert!(line.contains("Intel UHD Graphics"), "{line}");
        assert!(line.contains("i7-10610U"), "{line}");
    }

    #[test]
    fn specs_line_drops_missing_optionals() {
        let mut host = sample_host();
        host.hardware = Some(HostHardware {
            cpu_cores: Some(8),
            mem_total_bytes: Some(8 * 1_073_741_824),
            ..HostHardware::default()
        });
        assert_eq!(specs_line(&host).as_deref(), Some("8 cores · 8.0 GB"));
    }

    #[test]
    fn hold_reason_says_running_jobs_finish() {
        let mut host = sample_host();
        host.paused = true;
        host.current_jobs = 3;
        let with_jobs = hold_jobs_reason(&host).expect("holding with jobs");
        assert_no_pause(&with_jobs);
        assert!(
            with_jobs.contains("already running will finish"),
            "{with_jobs}"
        );

        host.current_jobs = 0;
        let idle = hold_jobs_reason(&host).expect("holding idle");
        assert_no_pause(&idle);
        assert!(idle.contains("were not stopped"), "{idle}");

        host.paused = false;
        host.drain = true;
        host.current_jobs = 1;
        assert!(holding_new_jobs(&host));
        let drain = hold_jobs_reason(&host).expect("drain holds new jobs");
        assert!(drain.contains("already running will finish"), "{drain}");

        host.drain = false;
        assert!(hold_jobs_reason(&host).is_none());

        host.allow_host = false;
        host.allow_container = false;
        assert!(holding_new_jobs(&host));
        let both_off = hold_jobs_reason(&host).expect("both modes off holds");
        assert_no_pause(&both_off);
    }

    #[test]
    fn never_paired_disabled_hidden_from_removed_list() {
        let mut host = sample_host();
        host.status = "disabled".into();
        assert!(is_removed(&host));
        let only = [host.clone()];
        assert!(!in_removed_list(&host, &only));

        host.paired_at = Some("2026-01-01T00:00:00Z".into());
        let only = [host.clone()];
        assert!(in_removed_list(&host, &only));

        host.paired_at = Some("  ".into());
        host.last_seen_at = Some("2026-01-02T00:00:00Z".into());
        let only = [host.clone()];
        assert!(in_removed_list(&host, &only));

        host.last_seen_at = None;
        host.hostname = Some("box.local".into());
        let only = [host.clone()];
        assert!(in_removed_list(&host, &only));

        host.hostname = None;
        host.platform = Some("linux".into());
        let only = [host.clone()];
        assert!(in_removed_list(&host, &only));

        host.platform = Some(String::new());
        let only = [host.clone()];
        assert!(!in_removed_list(&host, &only));

        host.status = "pending".into();
        host.paired_at = None;
        assert!(!is_removed(&host));
        let only = [host.clone()];
        assert!(!in_removed_list(&host, &only));
    }

    #[test]
    fn removed_clone_of_live_box_is_hidden() {
        let mut live = sample_host();
        live.id = "live".into();
        live.status = "online".into();
        live.hostname = Some("x1".into());
        live.platform = Some("windows-x86_64".into());
        live.machine_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());

        let mut dead = sample_host();
        dead.id = "dead".into();
        dead.status = "disabled".into();
        dead.paired_at = Some("2026-01-01T00:00:00Z".into());
        dead.hostname = Some("x1".into());
        dead.platform = Some("windows-x86_64".into());

        let all = [live.clone(), dead.clone()];
        assert!(!in_removed_list(&dead, &all));

        dead.hostname = Some("other".into());
        dead.machine_id = live.machine_id.clone();
        let all = [live.clone(), dead.clone()];
        assert!(!in_removed_list(&dead, &all));
    }

    #[test]
    fn older_removed_duplicate_is_hidden() {
        let mut older = sample_host();
        older.id = "old".into();
        older.status = "disabled".into();
        older.hostname = Some("x1".into());
        older.platform = Some("windows-x86_64".into());
        older.paired_at = Some("2026-01-01T00:00:00Z".into());
        older.last_seen_at = Some("2026-01-01T00:00:00Z".into());

        let mut newer = older.clone();
        newer.id = "new".into();
        newer.last_seen_at = Some("2026-02-01T00:00:00Z".into());

        let all = [older.clone(), newer.clone()];
        assert!(!in_removed_list(&older, &all));
        assert!(in_removed_list(&newer, &all));
    }

    /// catalog:list-hide-removed-never-paired
    /// catalog:unit-hide-removed — Show / hide removed; hide clones + never-paired.
    /// Never-paired cancel is not a removed card. Revoked paired box appears
    /// when Show removed. Re-pair hides the old clone.
    #[test]
    fn in_removed_list_hides_clones_and_never_paired() {
        // just test excludes fleet-ui; lock the live list split so swapping
        // is_removed onto Show removed (or in_removed_list onto the default
        // list) cannot go green. Never-paired cancel must vanish, not become
        // a removed card; revoked paired stays off the default list.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("in_removed_list"),
            "production source must import in_removed_list"
        );
        assert!(
            import.contains("is_removed"),
            "production source must import is_removed"
        );
        assert!(
            !prod.contains("fn in_removed_list"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert!(
            prod.contains("filter(|h| !is_removed(h))"),
            "default list must hide every disabled row (Cancel leftover is discarded)"
        );
        assert!(
            prod.contains("filter(|h| in_removed_list(h, &all))"),
            "Show removed must use in_removed_list, not every disabled row"
        );
        assert!(
            prod.contains("Show removed ({removed_n})"),
            "Show removed (N) must count in_removed_list cards only"
        );
        assert!(
            prod.contains("Hide removed ({removed_n})"),
            "Hide removed (N) must count in_removed_list cards only"
        );
        assert!(
            prod.contains("if removed_n > 0"),
            "Show removed must stay hidden until a paired box is revoked"
        );
        assert!(
            prod.contains("if show_removed()"),
            "removed cards render only after Show removed"
        );

        fn page_lists(all: &[HostPublic]) -> (Vec<String>, Vec<String>) {
            let active: Vec<String> = all
                .iter()
                .filter(|h| !is_removed(h))
                .map(|h| h.id.clone())
                .collect();
            let removed: Vec<String> = all
                .iter()
                .filter(|h| in_removed_list(h, all))
                .map(|h| h.id.clone())
                .collect();
            (active, removed)
        }

        let mut leftover = sample_host();
        leftover.id = "leftover".into();
        leftover.name = "never-paired-cancel".into();
        leftover.kind = "host".into();
        leftover.status = "disabled".into();
        leftover.paired_at = None;
        leftover.last_seen_at = None;
        leftover.hostname = None;
        leftover.platform = None;
        leftover.machine_id = None;
        leftover.created_at = Some("2026-01-01T00:00:00Z".into());
        assert!(is_removed(&leftover));
        assert_eq!(status_label(&leftover.status), "removed");
        assert!(
            !in_removed_list(&leftover, std::slice::from_ref(&leftover)),
            "never-paired cancel is not a removed card"
        );

        let mut live = sample_host();
        live.id = "live".into();
        live.name = "paired-box".into();
        live.kind = "host".into();
        live.status = "online".into();
        live.paired_at = Some("2026-01-01T00:00:00Z".into());
        live.last_seen_at = Some("2026-03-01T00:00:00Z".into());
        live.hostname = Some("x1".into());
        live.platform = Some("windows-x86_64".into());
        live.machine_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());

        // Leftover + live paired: no Show removed. Cancel leftover is gone.
        let all = [leftover.clone(), live.clone()];
        let (active, removed) = page_lists(&all);
        assert_eq!(active, vec!["live".to_string()]);
        assert!(
            removed.is_empty(),
            "Show removed stays hidden until a paired box is revoked; leftover is not a card: {removed:?}"
        );
        assert!(!in_removed_list(&leftover, &all));
        assert!(!in_removed_list(&live, &all));

        // Revoke the paired box: hidden by default, appears under Show removed.
        // Never-paired leftover still does not increment the count.
        let mut revoked = live.clone();
        revoked.status = "disabled".into();
        let all = [leftover.clone(), revoked.clone()];
        let (active, removed) = page_lists(&all);
        assert!(
            active.is_empty(),
            "revoked paired box is hidden until Show removed: {active:?}"
        );
        assert_eq!(
            removed,
            vec!["live".to_string()],
            "Show removed after revoke of a paired box; leftover stays hidden: {removed:?}"
        );
        assert!(in_removed_list(&revoked, &all));
        assert!(!in_removed_list(&leftover, &all));
        assert_eq!(status_label(&revoked.status), "removed");

        // Blank identity fields are still never-paired (Cancel leftover).
        leftover.hostname = Some("  ".into());
        leftover.platform = Some("".into());
        leftover.paired_at = Some("   ".into());
        leftover.last_seen_at = Some(" ".into());
        let all = [leftover.clone(), revoked.clone()];
        assert!(
            !in_removed_list(&leftover, &all),
            "whitespace hostname/platform/paired_at is still never-paired"
        );
        leftover.hostname = None;
        leftover.platform = None;
        leftover.paired_at = None;
        leftover.last_seen_at = None;

        // Re-pair of the same box hides the old clone. Leftover stays gone.
        let mut clone = revoked.clone();
        clone.id = "old-clone".into();
        clone.hostname = Some("X1".into());
        clone.platform = Some("WINDOWS-x86_64".into());
        let mut repaired = live.clone();
        repaired.id = "repaired".into();
        repaired.status = "online".into();
        let all = [leftover.clone(), clone.clone(), repaired.clone()];
        let (active, removed) = page_lists(&all);
        assert_eq!(active, vec!["repaired".to_string()]);
        assert!(
            removed.is_empty(),
            "re-pair hides the old clone; leftover is not a removed card: {removed:?}"
        );
        assert!(!in_removed_list(&clone, &all));
        assert!(!in_removed_list(&repaired, &all));
        assert!(!in_removed_list(&leftover, &all));

        // Same machine_id is the same box even when hostname/platform differ.
        clone.hostname = Some("other".into());
        clone.platform = Some("linux-x86_64".into());
        clone.machine_id = Some("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE".into());
        let all = [clone.clone(), repaired.clone()];
        assert!(
            !in_removed_list(&clone, &all),
            "same machine_id (case-insensitive) is a clone of the live box"
        );

        // Same hostname + different platform is a different box, not a clone.
        let mut win = revoked.clone();
        win.id = "win-dead".into();
        win.hostname = Some("lab".into());
        win.platform = Some("windows-x86_64".into());
        win.machine_id = None;
        win.paired_at = Some("2026-01-01T00:00:00Z".into());
        let mut linux = win.clone();
        linux.id = "linux-dead".into();
        linux.platform = Some("linux-x86_64".into());
        linux.last_seen_at = Some("2026-01-02T00:00:00Z".into());
        let all = [win.clone(), linux.clone()];
        assert!(
            in_removed_list(&win, &all) && in_removed_list(&linux, &all),
            "same hostname on different OS is not a clone"
        );

        // Same platform with empty hostname is not a clone (no identity to match).
        win.hostname = None;
        linux.hostname = None;
        linux.platform = win.platform.clone();
        let all = [win.clone(), linux.clone()];
        assert!(
            in_removed_list(&win, &all) && in_removed_list(&linux, &all),
            "empty hostname + same platform is not a clone"
        );

        // An unrelated revoked paired box still appears when Show removed.
        let mut other = sample_host();
        other.id = "other-dead".into();
        other.name = "other-paired".into();
        other.kind = "host".into();
        other.status = "disabled".into();
        other.paired_at = Some("2026-02-01T00:00:00Z".into());
        other.last_seen_at = Some("2026-02-02T00:00:00Z".into());
        other.hostname = Some("lab-2".into());
        other.platform = Some("linux-x86_64".into());
        other.machine_id = Some("bbbbbbbb-cccc-dddd-eeee-ffffffffffff".into());
        let all = [
            leftover.clone(),
            clone.clone(),
            repaired.clone(),
            other.clone(),
        ];
        let (active, removed) = page_lists(&all);
        assert_eq!(active, vec!["repaired".to_string()]);
        assert_eq!(
            removed,
            vec!["other-dead".to_string()],
            "unrelated revoked paired box is the only Show-removed card"
        );
        assert!(in_removed_list(&other, &all));
        assert!(!in_removed_list(&clone, &all));
        assert!(!in_removed_list(&leftover, &all));

        // Two removed rows for the same box: only the newest is a card.
        let mut older = clone.clone();
        older.id = "old".into();
        older.status = "disabled".into();
        older.hostname = Some("x1".into());
        older.platform = Some("windows-x86_64".into());
        older.machine_id = None;
        older.paired_at = Some("2026-01-01T00:00:00Z".into());
        older.last_seen_at = Some("2026-01-01T00:00:00Z".into());
        let mut newer = older.clone();
        newer.id = "new".into();
        newer.last_seen_at = Some("2026-02-01T00:00:00Z".into());
        let all = [older.clone(), newer.clone()];
        assert!(!in_removed_list(&older, &all));
        assert!(in_removed_list(&newer, &all));

        // Unused pending (not yet cancelled) is waiting to pair, not removed.
        let mut pending = leftover.clone();
        pending.id = "pending".into();
        pending.status = "pending".into();
        assert!(!is_removed(&pending));
        assert!(!in_removed_list(&pending, std::slice::from_ref(&pending)));
    }

    fn sample_served(platform: &str, host: &str, driver: &str) -> HostDesiredSoftware {
        HostDesiredSoftware {
            platform: Some(platform.into()),
            git_sha: Some("ignored-bake-stamp".into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": host},
                "fleet-driver": {"sha256": driver}
            })),
            ..HostDesiredSoftware::default()
        }
    }

    #[test]
    fn served_platforms_list_file_hashes_not_bake_id() {
        let inv = PackageInventory {
            plane_git_sha: Some("planeplaneplaneplane".into()),
            packages: vec![
                sample_served(
                    "windows-x86_64",
                    "8c015655431b5287e58eb79e26d29647a8a4488d797d8c436ae4bd8181db3f58",
                    "6bec5b00654e94f1a469f1f7cee6f939413b6e36a942b444e798439fb642309d",
                ),
                sample_served("linux-x86_64", "aaaaaaaaaaaa1111", "bbbbbbbbbbbb2222"),
                HostDesiredSoftware {
                    platform: Some("linux-armv7".into()),
                    git_sha: Some("old-stamp-only".into()),
                    ..HostDesiredSoftware::default()
                },
            ],
            ..PackageInventory::default()
        };
        let served = served_platforms(&inv);
        assert_eq!(served.len(), 2, "no-bin platform is omitted");
        assert_eq!(served[0].platform, "linux-x86_64");
        assert_eq!(served[0].host, "aaaaaaaaaaaa");
        assert_eq!(served[0].driver, "bbbbbbbbbbbb");
        assert_eq!(served[1].platform, "windows-x86_64");
        assert_eq!(served[1].host, "8c015655431b");
        assert_eq!(served[1].driver, "6bec5b00654e");
        let blob = format!("{served:?}");
        assert!(!blob.contains("ignored-bake-stamp"));
        assert!(!blob.contains("planeplane"));
    }

    #[test]
    fn served_platforms_empty_inventory() {
        assert!(served_platforms(&PackageInventory::default()).is_empty());
    }

    #[test]
    fn stale_package_platforms_flags_missing_and_old_versions() {
        let inv = PackageInventory {
            package_versions: PackageVersions {
                host: "0.0.2".into(),
                driver: "0.0.2".into(),
            },
            packages: vec![
                HostDesiredSoftware {
                    platform: Some("linux-x86_64".into()),
                    bins: Some(serde_json::json!({
                        "fleet-host": {"sha256": "aaa", "version": "0.0.2"},
                        "fleet-driver": {"sha256": "bbb", "version": "0.0.2"}
                    })),
                    ..HostDesiredSoftware::default()
                },
                HostDesiredSoftware {
                    platform: Some("windows-x86_64".into()),
                    bins: Some(serde_json::json!({
                        "fleet-host.exe": {"sha256": "ccc", "version": "0.0.1"},
                        "fleet-driver.exe": {"sha256": "ddd", "version": "0.0.2"}
                    })),
                    ..HostDesiredSoftware::default()
                },
            ],
            ..PackageInventory::default()
        };
        let stale = stale_package_platforms(&inv);
        assert!(stale.iter().any(|s| s.platform == "windows-x86_64"));
        assert!(
            !stale.iter().any(|s| s.platform == "linux-aarch64"),
            "never-staged OS must not warn: {stale:?}"
        );
        assert!(!stale.iter().any(|s| s.platform == "linux-x86_64"));
        let warn = stale_package_warning(&inv).expect("warn");
        assert!(warn.contains("0.0.2"), "{warn}");
        assert!(warn.contains("windows-x86_64"), "{warn}");
        assert!(
            !warn.contains("linux-aarch64"),
            "never-staged OS must not warn: {warn}"
        );
        assert!(!warn.contains("linux-x86_64,"), "{warn}");
    }

    #[test]
    fn stale_package_warning_none_when_every_os_is_current() {
        let packages = REQUIRED_PACKAGE_PLATFORMS
            .iter()
            .map(|plat| HostDesiredSoftware {
                platform: Some((*plat).into()),
                bins: Some(serde_json::json!({
                    "fleet-host": {"sha256": "h", "version": "0.0.2"},
                    "fleet-driver": {"sha256": "d", "version": "0.0.2"}
                })),
                ..HostDesiredSoftware::default()
            })
            .collect();
        let inv = PackageInventory {
            package_versions: PackageVersions {
                host: "0.0.2".into(),
                driver: "0.0.2".into(),
            },
            packages,
            ..PackageInventory::default()
        };
        assert!(stale_package_platforms(&inv).is_empty());
        assert!(stale_package_warning(&inv).is_none());
    }

    /// catalog:list-header-current-host-driver-version
    /// Header is Current fleet-host version / Current fleet-driver version
    /// from runner-packages.toml [verify] via package_versions. Never a bake
    /// SHA or "Current runner build". Distinct from stale_package_warning
    /// and plane /health.git_sha.
    #[test]
    fn package_header_is_current_host_driver_version_from_verify_not_bake_sha() {
        fn toml_section_value(raw: &str, section: &str, key: &str) -> String {
            let header = format!("[{section}]");
            let needle = format!("{key} = \"");
            let mut in_section = false;
            for line in raw.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_section = line == header;
                    continue;
                }
                if !in_section {
                    continue;
                }
                if let Some(rest) = line.strip_prefix(needle.as_str()) {
                    if let Some(end) = rest.find('"') {
                        let v = &rest[..end];
                        if !v.is_empty() {
                            return v.to_string();
                        }
                    }
                }
            }
            String::new()
        }

        fn is_40_hex_git_sha(s: &str) -> bool {
            let s = s.trim();
            let hex = s.split_once('-').map(|(h, _)| h).unwrap_or(s);
            hex.len() == 40 && hex.chars().all(|c| c.is_ascii_hexdigit())
        }

        let toml = include_str!("../../../runner-packages.toml");
        assert!(
            toml.contains("[verify]"),
            "runner-packages.toml must have a [verify] section for the header"
        );
        let verify_host = toml_section_value(toml, "verify", "fleet-host");
        let verify_driver = toml_section_value(toml, "verify", "fleet-driver");
        assert!(
            looks_like_package_semver(&verify_host),
            "[verify] fleet-host must be package semver, not bake SHA: {verify_host:?}"
        );
        assert!(
            looks_like_package_semver(&verify_driver),
            "[verify] fleet-driver must be package semver, not bake SHA: {verify_driver:?}"
        );
        assert!(
            !is_40_hex_git_sha(&verify_host) && !is_file_hash(&verify_host),
            "[verify] fleet-host is a bake/file hash, not a version: {verify_host}"
        );
        assert!(
            !is_40_hex_git_sha(&verify_driver) && !is_file_hash(&verify_driver),
            "[verify] fleet-driver is a bake/file hash, not a version: {verify_driver}"
        );
        if let Some(sha) = option_env!("FLEET_GIT_SHA")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            assert_ne!(
                verify_host, sha,
                "[verify] fleet-host must not be FLEET_GIT_SHA /health.git_sha"
            );
            assert_ne!(
                verify_driver, sha,
                "[verify] fleet-driver must not be FLEET_GIT_SHA /health.git_sha"
            );
        }

        // Header reads package_versions, which the plane fills from [verify]
        // — not [versions] (staging) and not plane_git_sha.
        let from_verify = PackageVersions {
            host: verify_host.clone(),
            driver: verify_driver.clone(),
        };
        let header = package_header_lines(&from_verify);
        assert_eq!(
            header,
            vec![
                format!("{PACKAGE_HEADER_HOST_PREFIX} {verify_host}"),
                format!("{PACKAGE_HEADER_DRIVER_PREFIX} {verify_driver}"),
            ]
        );
        assert_eq!(
            package_header_host_line(&from_verify).as_deref(),
            Some(header[0].as_str())
        );
        assert_eq!(
            package_header_driver_line(&from_verify).as_deref(),
            Some(header[1].as_str())
        );
        let blob = header.join("\n");
        assert!(
            !blob.contains("Current runner build"),
            "header must not use the retired Current runner build label: {blob}"
        );
        assert!(
            !blob.contains("linux-") && !blob.contains("windows-"),
            "header is one [verify] number, not a per-OS served list: {blob}"
        );
        for line in &header {
            for tok in line.split_whitespace() {
                assert!(
                    !is_40_hex_git_sha(tok) && !is_file_hash(tok),
                    "header must not show a bake SHA: {line}"
                );
            }
        }

        // A bake SHA in package_versions is not a runner version — omit the line.
        let bake = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351";
        let dirty = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351-e8260cd5c443";
        let file_hash = "8c015655431b5287e58eb79e26d29647a8a4488d797d8c436ae4bd8181db3f58";
        for bad in [bake, dirty, file_hash, "9d239a509055", "", "  "] {
            let only_bad = PackageVersions {
                host: bad.into(),
                driver: bad.into(),
            };
            assert!(
                package_header_lines(&only_bad).is_empty(),
                "bake/file-hash/empty {bad:?} must not become a header line"
            );
        }
        let mixed = PackageVersions {
            host: verify_host.clone(),
            driver: bake.into(),
        };
        assert_eq!(
            package_header_lines(&mixed),
            vec![format!("{PACKAGE_HEADER_HOST_PREFIX} {verify_host}")],
            "driver bake SHA must not leak onto the header next to a real host version"
        );
        if let Some(sha) = option_env!("FLEET_GIT_SHA")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let baked = PackageVersions {
                host: sha.into(),
                driver: sha.into(),
            };
            assert!(
                package_header_lines(&baked).is_empty(),
                "FLEET_GIT_SHA {sha} is /health.git_sha, not a header version"
            );
        }

        // Distinct from the amber stale-OS banner.
        let stale_inv = PackageInventory {
            package_versions: from_verify.clone(),
            plane_git_sha: Some(bake.into()),
            packages: vec![HostDesiredSoftware {
                platform: Some("windows-x86_64".into()),
                git_sha: Some(dirty.into()),
                bins: Some(serde_json::json!({
                    "fleet-host": {"sha256": file_hash, "version": "0.0.1"},
                    "fleet-driver": {"sha256": file_hash, "version": "0.0.1"}
                })),
                ..HostDesiredSoftware::default()
            }],
            ..PackageInventory::default()
        };
        let warn = stale_package_warning(&stale_inv).expect("stale OS banner is a sibling");
        assert!(warn.contains("windows-x86_64"), "{warn}");
        assert_ne!(
            package_header_lines(&stale_inv.package_versions).join(" "),
            warn,
            "header labels are not the stale-OS banner"
        );
        assert!(
            !package_header_lines(&stale_inv.package_versions)
                .iter()
                .any(|l| l.contains("windows-x86_64") || l.contains(file_hash)),
            "header must not list stale OS or bin SHA"
        );

        // just test excludes fleet-ui; lock the live RSX so swapping the
        // labels back to Current runner build, interpolating plane_git_sha,
        // or dropping the helper still fails on the default unit path.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("package_header_host_line"),
            "production source must import package_header_host_line"
        );
        assert!(
            import.contains("package_header_driver_line"),
            "production source must import package_header_driver_line"
        );
        assert!(
            !prod.contains("fn package_header_host_line")
                && !prod.contains("fn package_header_driver_line")
                && !prod.contains("fn package_header_lines"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert!(
            prod.contains("package_header_host_line(&pkg_ver)"),
            "header must render package_header_host_line(&pkg_ver)"
        );
        assert!(
            prod.contains("package_header_driver_line(&pkg_ver)"),
            "header must render package_header_driver_line(&pkg_ver)"
        );
        assert!(
            prod.contains("let pkg_ver = packages().package_versions"),
            "header values come from API package_versions ([verify]), not a compile-time bake"
        );
        assert!(
            !prod.contains("Current runner build"),
            "production RSX must not say Current runner build"
        );
        assert!(
            !prod.contains("\"Current fleet-host version {pkg_ver.host}\""),
            "raw pkg_ver.host interpolation would show a bake SHA; use the helper"
        );
        assert!(
            !prod.contains("\"Current fleet-driver version {pkg_ver.driver}\""),
            "raw pkg_ver.driver interpolation would show a bake SHA; use the helper"
        );
        let host_at = prod
            .find("package_header_host_line(&pkg_ver)")
            .expect("host header call");
        let driver_at = prod
            .find("package_header_driver_line(&pkg_ver)")
            .expect("driver header call");
        let stale_at = prod
            .find("if let Some(warn) = stale_builds")
            .expect("stale banner stays a sibling, not the header");
        assert!(
            host_at < driver_at && driver_at < stale_at,
            "header host/driver lines must render above the stale-OS banner"
        );
        assert!(
            prod.contains("stale_package_warning(&packages())"),
            "amber stale-OS banner must stay a sibling of the header, not replace it"
        );
        let header_block = &prod[host_at..stale_at];
        for needle in [
            "plane_git_sha",
            "software_build_id",
            "git_sha",
            "FLEET_GIT_SHA",
        ] {
            assert!(
                !header_block.contains(needle),
                "header must not interpolate {needle} (that is /health.git_sha, not [verify])"
            );
        }
        assert!(
            prod.contains("[verify] in runner-packages.toml"),
            "header title must say the number is [verify], not a bake SHA"
        );
    }

    #[test]
    fn agent_status_phrase_matches_runners_and_chat() {
        assert_eq!(
            agent_status_phrase("claude", true, true),
            "installed · logged in"
        );
        assert_eq!(
            agent_status_phrase("claude", true, false),
            "installed · logged out"
        );
        assert_eq!(agent_status_phrase("claude", false, false), "not installed");
        assert_eq!(agent_status_phrase("ollama", true, false), "installed");
        assert_eq!(agent_status_phrase("ollama", false, false), "not installed");
    }

    #[test]
    fn ollama_catalog_label_is_private() {
        assert_eq!(catalog_provider_display_name("ollama"), "Private (Ollama)");
        assert_eq!(catalog_provider_display_name("grok"), "Grok");
    }

    #[test]
    fn claude_aliases_are_not_version_pins() {
        for name in claude_cli_model_aliases() {
            assert!(!name.chars().any(|c| c.is_ascii_digit()), "{name}");
        }
        assert!(claude_effort_values().contains(&"xhigh"));
    }

    #[test]
    fn chat_elapsed_formats() {
        assert_eq!(format_chat_elapsed(240), "240 ms");
        assert_eq!(format_chat_elapsed(1500), "1.5s");
        assert_eq!(format_chat_elapsed(12_400), "12s");
        assert_eq!(format_chat_elapsed(63_000), "1m 03s");
    }

    fn cand(id: &str, available: bool, models: &[&str]) -> ChatProviderCandidate {
        ChatProviderCandidate {
            id: id.into(),
            available,
            models: models.iter().map(|s| (*s).to_string()).collect(),
            default_model: models.first().map(|s| (*s).to_string()),
        }
    }

    #[test]
    fn chat_defaults_prefer_last_used_when_available() {
        let providers = vec![
            cand("grok", true, &["grok-4.5"]),
            cand("ollama", true, &["llama3.2:latest"]),
        ];
        let pick =
            pick_chat_provider_and_model(&providers, Some("ollama"), Some("llama3.2:latest"))
                .expect("pick");
        assert_eq!(pick.provider_id, "ollama");
        assert_eq!(pick.model, "llama3.2:latest");
    }

    #[test]
    fn chat_defaults_skip_last_used_when_not_ready() {
        let providers = vec![cand("grok", false, &[]), cand("ollama", true, &["qwen2.5"])];
        let pick =
            pick_chat_provider_and_model(&providers, Some("grok"), Some("grok-4.5")).expect("pick");
        assert_eq!(pick.provider_id, "ollama");
        assert_eq!(pick.model, "qwen2.5");
    }

    #[test]
    fn chat_defaults_do_not_override_user_pick() {
        let providers = vec![cand("grok", false, &[]), cand("ollama", true, &["qwen2.5"])];
        assert!(!should_apply_chat_defaults(&providers, "grok", true));
        assert!(should_apply_chat_defaults(&providers, "grok", false));
        assert!(!should_apply_chat_defaults(&providers, "ollama", false));
        assert!(should_apply_chat_defaults(&providers, "", false));
    }

    fn detail(name: &str, efforts: &[&str], default: &str) -> crate::types::AgentModelDetail {
        crate::types::AgentModelDetail {
            name: name.into(),
            efforts: efforts
                .iter()
                .map(|v| crate::types::AgentEffort {
                    value: (*v).into(),
                    label: None,
                })
                .collect(),
            default_effort: Some(default.into()),
        }
    }

    #[test]
    fn effort_picker_uses_last_used_when_model_still_lists_it() {
        let details = vec![detail(
            "grok-4.6",
            &["xhigh", "high", "medium", "low"],
            "high",
        )];
        let d = find_model_detail(&details, "grok-4.6");
        assert_eq!(
            pick_effort_value(d, Some("xhigh")).as_deref(),
            Some("xhigh")
        );
    }

    #[test]
    fn effort_picker_falls_back_to_model_default() {
        let details = vec![detail(
            "grok-4.6",
            &["xhigh", "high", "medium", "low"],
            "high",
        )];
        let d = find_model_detail(&details, "grok-4.6");
        assert_eq!(
            pick_effort_value(d, Some("medium")).as_deref(),
            Some("medium")
        );
        assert_eq!(pick_effort_value(d, Some("nope")).as_deref(), Some("high"));
        assert_eq!(
            pick_effort_value(find_model_detail(&details, ""), None).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn effort_picker_hidden_when_model_has_no_menu() {
        let details = vec![detail("sxs-claude-opus-4-6", &[], "high")];
        assert!(
            pick_effort_value(
                find_model_detail(&details, "sxs-claude-opus-4-6"),
                Some("high")
            )
            .is_none()
        );
    }

    #[test]
    fn software_never_reported_is_unknown() {
        assert_eq!(software_line(&sample_host()), "unknown");
    }

    #[test]
    fn software_current_shows_build_sha_and_os() {
        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());
        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","bins":{"fleet-host":"2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"}}"#,
        )
        .expect("software");
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software_status = Some("current".into());
        // Hash-only report + current versions is not "up to date" — the box
        // has not named a package version. The two bin rows are the status.
        assert_ne!(software_flag(&host), Some("up to date"));
        assert!(!software_line(&host).contains("2a971d69"));
        assert!(!software_line(&host).contains("9d239a509055"));
        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","bins":{"fleet-host":"aaaaaaaaaaaa1111","fleet-driver":"bbbbbbbbbbbb2222"}}"#,
        )
        .expect("software");
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"aaaaaaaaaaaa1111","version":"0.0.1"},"fleet-driver":{"sha256":"bbbbbbbbbbbb2222","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            git_sha: Some("9d239a509055abcdef".into()),
            platform: Some("linux-x86_64".into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": "aaaaaaaaaaaa1111", "version": "0.0.1"},
                "fleet-driver": {"sha256": "bbbbbbbbbbbb2222", "version": "0.0.1"}
            })),
            ..HostDesiredSoftware::default()
        });
        let rows = software_bin_rows(&host);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "fleet-host");
        assert_eq!(rows[0].version, "0.0.1");
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[1].name, "fleet-driver");
        assert_eq!(rows[1].version, "0.0.1");
        assert_eq!(rows[1].status, "current");
    }

    #[test]
    fn software_bin_rows_show_reported_semver_when_behind() {
        let mut host = sample_host();
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"old","version":"0.0.1"},"fleet-driver":{"sha256":"old","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": "new", "version": "0.0.2"},
                "fleet-driver": {"sha256": "new", "version": "0.0.2"}
            })),
            ..HostDesiredSoftware::default()
        });
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].version, "0.0.1");
        assert_eq!(rows[0].status, "behind");
        assert_eq!(rows[1].version, "0.0.1");
        assert_eq!(rows[1].status, "behind");
        assert!(!rows[0].version.contains("old"));
    }

    #[test]
    fn software_bin_rows_current_from_package_versions_without_desired() {
        let mut host = sample_host();
        host.desired = None;
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"cargo-host","version":"0.0.1"},"fleet-driver":{"sha256":"cargo-driver","version":"0.0.1"}}}"#,
        )
        .expect("software");
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[1].status, "current");
        assert_eq!(software_flag(&host), Some("up to date"));
    }

    #[test]
    fn software_bin_rows_same_version_different_hash_is_current() {
        let mut host = sample_host();
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"cargo","version":"0.0.1"},"fleet-driver":{"sha256":"cargo-d","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": "staged", "version": "0.0.1"},
                "fleet-driver": {"sha256": "staged-d", "version": "0.0.1"}
            })),
            ..HostDesiredSoftware::default()
        });
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[1].status, "current");
        assert_eq!(rows[0].version, "0.0.1");
        assert_eq!(software_flag(&host), Some("up to date"));
    }

    #[test]
    fn software_bin_rows_flag_driver_mismatch() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.software = serde_json::from_str(
            r#"{"git_sha":"bake","bins":{"fleet-host":"aaaaaaaaaaaa1111","fleet-driver":"oldoldoldold9999"}}"#,
        )
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            bins: Some(serde_json::json!({
                "fleet-host.exe": {"sha256": "aaaaaaaaaaaa1111"},
                "fleet-driver.exe": {"sha256": "bbbbbbbbbbbb2222"}
            })),
            ..HostDesiredSoftware::default()
        });
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[1].status, "behind");
        assert_eq!(rows[1].name, "fleet-driver");
    }

    #[test]
    fn software_matching_served_windows_package_is_up_to_date() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.software = serde_json::from_str(
            r#"{"git_sha":"18a1be7df0a53136e7a85b741d8b9fc45b4cc351-e8260cd5c443"}"#,
        )
        .expect("software");
        host.software_status = Some("current".into());
        // Plane bake can differ (Linux server vs Windows restage). That must
        // not mark a box that matches the served Windows package as old.
        host.plane_git_sha = Some("18a1be7df0a53136e7a85b741d8b9fc45b4cc351-9f1c128acf45".into());
        // Same served-package hashes, different plane bake: still current
        // only after the box reports the package versions.
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"git_sha":"18a1be7df0a53136e7a85b741d8b9fc45b4cc351-e8260cd5c443","bins":{"fleet-host":{"sha256":"aaaaaaaaaaaa1111","version":"0.0.1"},"fleet-driver":{"sha256":"bbbbbbbbbbbb2222","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": "aaaaaaaaaaaa1111", "version": "0.0.1"},
                "fleet-driver": {"sha256": "bbbbbbbbbbbb2222", "version": "0.0.1"}
            })),
            ..HostDesiredSoftware::default()
        });
        assert_eq!(software_flag(&host), Some("up to date"));
        assert!(software_line(&host).contains("up to date"));
        assert!(!extra_status_pills(&host).contains(&"old package"));
    }

    #[test]
    fn software_current_without_bake_id_is_up_to_date() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e","version":"0.0.1"},"fleet-driver":{"sha256":"bbbb","version":"0.0.1"}}}"#,
        )
        .expect("software");
        assert_eq!(software_line(&host), "windows-x86_64 · up to date");
        assert_eq!(software_flag(&host), Some("up to date"));
    }

    #[test]
    fn software_outdated_keeps_build_sha() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.current_versions = PackageVersions {
            host: "0.0.2".into(),
            driver: "0.0.2".into(),
        };
        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","bins":{"fleet-host":{"sha256":"old","version":"0.0.1"},"fleet-driver":{"sha256":"old","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.auto_update = false;
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(software_line(&host), "windows-x86_64 · outdated");
        assert!(!software_line(&host).contains("9d239a509055"));
        assert!(!software_line(&host).contains("updating"));
    }

    #[test]
    fn software_auto_update_does_not_fake_updating() {
        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());
        host.current_versions = PackageVersions {
            host: "0.0.2".into(),
            driver: "0.0.2".into(),
        };
        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","bins":{"fleet-host":{"sha256":"old","version":"0.0.1"},"fleet-driver":{"sha256":"old","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.auto_update = true;
        host.current_jobs = 0;
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(software_line(&host), "linux-x86_64 · outdated");

        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","updating":true,"bins":{"fleet-host":{"sha256":"old","version":"0.0.1"},"fleet-driver":{"sha256":"old","version":"0.0.1"}}}"#,
        )
        .expect("software");
        assert_eq!(software_flag(&host), Some("updating"));
    }

    #[test]
    fn software_updating_beats_outdated() {
        let mut host = sample_host();
        host.software = serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef","updating":true}"#)
            .expect("software");
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert!(software_line(&host).ends_with("updating"));
    }

    /// catalog:unit-fleet-ui-helpers — Offline does not repeat a heartbeat
    /// sentence. Body has no “heartbeating” even when last_seen is recent.
    #[test]
    fn not_taking_reason_offline_no_heartbeat_sentence() {
        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "offline".into();

        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;

        // Trap: a helper that said "heartbeating" when last_seen was fresh
        // would pass a no-timestamp offline check. Offline is the status
        // pill; last_seen stays "just now" and must not become a sentence.
        host.last_seen_at = Some("2026-08-20T12:00:00Z".into());
        assert_eq!(last_seen_label(&host, now_ms), "just now");
        assert_eq!(status_label(&host.status), "offline");
        assert!(
            not_taking_reason(&host).is_none(),
            "offline must not add a not-taking sentence: {:?}",
            not_taking_reason(&host)
        );
        let copy = card_copy(&host, now_ms);
        assert!(copy.contains("offline"), "{copy}");
        assert!(copy.contains("just now"), "{copy}");
        assert_no_heartbeating(&copy);

        // Still-fresh last_seen (seconds ago, inside a 90s window) is the
        // same lock — status offline must not flip the body to heartbeating.
        host.last_seen_at = Some("2026-08-20T11:59:30Z".into());
        assert_eq!(last_seen_label(&host, now_ms), "30s ago");
        assert!(
            not_taking_reason(&host).is_none(),
            "offline + 30s last_seen must stay silent: {:?}",
            not_taking_reason(&host)
        );
        assert_no_heartbeating(&card_copy(&host, now_ms));

        host.last_seen_at = Some("2026-08-20T11:59:01Z".into());
        assert_eq!(last_seen_label(&host, now_ms), "59s ago");
        assert!(not_taking_reason(&host).is_none());
        assert_no_heartbeating(&card_copy(&host, now_ms));

        // Hold / drain / last_error / running jobs must not invent a
        // heartbeating line. Offline wins the match before hold_jobs_reason.
        host.last_seen_at = Some("2026-08-20T12:00:00Z".into());
        host.paused = true;
        host.drain = true;
        host.allow_host = false;
        host.allow_container = false;
        host.advertised_host = false;
        host.current_jobs = 3;
        host.last_error = Some("disk full".into());
        assert!(holding_new_jobs(&host));
        assert!(
            not_taking_reason(&host).is_none(),
            "offline still wins over hold/error: {:?}",
            not_taking_reason(&host)
        );
        assert_no_heartbeating(&card_copy(&host, now_ms));

        // Never seen — last-seen chip is "never", still no heartbeat sentence.
        host.last_seen_at = None;
        assert_eq!(last_seen_label(&host, now_ms), "never");
        assert!(not_taking_reason(&host).is_none());
        assert_no_heartbeating(&card_copy(&host, now_ms));

        // Control: other statuses still emit their own sentence, never
        // "heartbeating". Proves we did not make not_taking_reason always None.
        host.status = "pending".into();
        assert_eq!(
            not_taking_reason(&host).as_deref(),
            Some("Waiting to pair.")
        );
        assert_no_heartbeating(&card_copy(&host, now_ms));
    }

    #[test]
    fn control_plane_sorts_first() {
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.name = "Control plane".into();
        let mut other = sample_host();
        other.id = "h2".into();
        other.status = "online".into();
        other.kind = "host".into();
        assert!(is_control_plane(&plane));
        assert!(!is_control_plane(&other));
        let mut active = vec![other.clone(), plane.clone()];
        sort_hosts_for_list(&mut active, 1_700_000_000_000.0);
        assert_eq!(active[0].id, plane.id);
    }

    /// catalog:unit-extra-status-pills — settings_gen != ack → pending apply;
    /// behind package → outdated; control plane has neither.
    #[test]
    fn extra_status_pills_includes_pending_apply_and_outdated() {
        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.settings_pending = false;
        host.outdated = false;
        host.software_status = None;

        set_package_versions(&mut host, "0.0.2", "0.0.2");
        host.settings_gen = 1;
        host.settings_ack_gen = 1;
        assert!(!settings_awaiting_ack(&host));
        assert_eq!(software_flag(&host), Some("up to date"));
        assert!(
            extra_status_pills(&host).is_empty(),
            "current package + acked gen must show no extra pills: {:?}",
            extra_status_pills(&host)
        );

        // Card: settings_gen != ack → pending apply. Do not rely on
        // settings_pending; that flag can lag or stay set after a bad ack.
        host.settings_gen = 2;
        host.settings_ack_gen = 1;
        host.settings_pending = false;
        assert!(settings_awaiting_ack(&host));
        assert_eq!(extra_status_pills(&host), vec!["pending apply"]);

        // API outdated/behind bits without a behind package must not fake
        // the outdated chip. The pill is the package compare.
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert_eq!(extra_status_pills(&host), vec!["pending apply"]);

        set_package_versions(&mut host, "0.0.1", "0.0.2");
        host.outdated = false;
        host.software_status = None;
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(extra_status_pills(&host), vec!["pending apply", "outdated"]);

        // Ack clears pending apply; behind package keeps outdated.
        host.settings_gen = 2;
        host.settings_ack_gen = 2;
        host.settings_pending = false;
        assert!(!settings_awaiting_ack(&host));
        assert_eq!(extra_status_pills(&host), vec!["outdated"]);

        // Unpaired / removed cards still show outdated when behind; they
        // have not Applied, so no pending-apply chip.
        host.settings_gen = 2;
        host.settings_ack_gen = 1;
        host.status = "pending".into();
        assert_eq!(extra_status_pills(&host), vec!["outdated"]);
        host.status = "disabled".into();
        assert_eq!(extra_status_pills(&host), vec!["outdated"]);

        // Plane apply is immediate and this process is not a host package.
        let mut plane = host.clone();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.settings_gen = 4;
        plane.settings_ack_gen = 1;
        plane.settings_pending = true;
        plane.outdated = true;
        plane.software_status = Some("behind".into());
        set_package_versions(&mut plane, "0.0.1", "0.0.2");
        assert!(is_control_plane(&plane));
        assert!(settings_awaiting_ack(&plane));
        assert_eq!(software_flag(&plane), Some("outdated"));
        assert!(
            extra_status_pills(&plane).is_empty(),
            "control plane must not show pending apply or outdated: {:?}",
            extra_status_pills(&plane)
        );
    }

    #[test]
    fn extra_pills_skip_control_plane() {
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.settings_gen = 3;
        plane.settings_ack_gen = 1;
        plane.settings_pending = true;
        plane.outdated = true;
        plane.software_status = Some("behind".into());
        assert!(settings_awaiting_ack(&plane));
        assert!(extra_status_pills(&plane).is_empty());
    }

    #[test]
    fn status_label_maps_disabled_to_removed() {
        assert_eq!(status_label("disabled"), "removed");
        assert_eq!(status_label("online"), "online");
        assert_eq!(status_label("offline"), "offline");
        assert_eq!(status_label("pending"), "pending");
        assert_eq!(status_label("error"), "error");
    }

    /// catalog:list-public-status-offline-after-90s
    /// catalog:unit-public-status — Pills: online, offline, pending, removed, error.
    /// Stored online + 91s silent → offline. Pending / removed / error keep
    /// their status; the 90s rule must not overwrite them.
    #[test]
    fn public_status_is_offline_when_last_seen_older_than_90s() {
        // just test excludes fleet-ui; lock the live pills so swapping
        // status_label(&host.status) back onto the card cannot go green.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("public_status"),
            "production source must import public_status"
        );
        assert!(
            !prod.contains("fn public_status"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert!(
            prod.contains("public_status(&host, now_ms)"),
            "list + detail pills must call public_status, not raw host.status"
        );
        assert!(
            prod.contains("status_label(status)"),
            "pill text must be status_label(public_status(...))"
        );
        assert!(
            !prod.contains("status_label(&host.status)"),
            "pills must not render stored host.status without the 90s rule"
        );
        assert!(
            !prod.contains("status_pill_class(&host.status)"),
            "pill color must use public_status, not stored host.status"
        );

        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        let iso = |raw: &str| Some(raw.to_string());

        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();

        // Trap: returning host.status as-is would stay "online" after 91s.
        host.last_seen_at = iso("2026-08-20T11:58:29Z");
        assert_eq!(public_status(&host, now_ms), "offline");
        assert_eq!(status_label(public_status(&host, now_ms)), "offline");
        let copy = card_copy(&host, now_ms);
        assert!(copy.contains("offline"), "{copy}");
        assert!(
            !copy.split_whitespace().any(|w| w == "online"),
            "91s silent must not keep the online pill: {copy}"
        );

        // Exactly 90s is still online — a missed beat must not flicker.
        host.last_seen_at = iso("2026-08-20T11:58:30Z");
        assert_eq!(public_status(&host, now_ms), "online");
        assert_eq!(status_label(public_status(&host, now_ms)), "online");

        host.last_seen_at = iso("2026-08-20T11:59:00Z");
        assert_eq!(
            public_status(&host, now_ms),
            "online",
            "60s silent is inside the 90s window"
        );
        host.last_seen_at = iso("2026-08-20T11:59:55Z");
        assert_eq!(public_status(&host, now_ms), "online");
        host.last_seen_at = iso("2026-08-20T12:00:00Z");
        assert_eq!(public_status(&host, now_ms), "online");

        // Sub-second: 90.001s is older than 90s.
        host.last_seen_at = iso("2026-08-20T11:58:29.999Z");
        assert_eq!(public_status(&host, now_ms), "offline");
        host.last_seen_at = iso("2026-08-20T11:58:30.000Z");
        assert_eq!(public_status(&host, now_ms), "online");

        // Never seen / empty / unparseable online is offline, not a hung pill.
        host.last_seen_at = None;
        assert_eq!(public_status(&host, now_ms), "offline");
        host.last_seen_at = iso("");
        assert_eq!(public_status(&host, now_ms), "offline");
        host.last_seen_at = iso("   ");
        assert_eq!(public_status(&host, now_ms), "offline");
        host.last_seen_at = iso("not-a-timestamp");
        assert_eq!(public_status(&host, now_ms), "offline");

        // Trap: flipping any stale last_seen to offline would smash
        // pending / removed / error. The 90s rule is online-only.
        let stale = iso("2026-08-20T11:58:29Z");
        for (stored, pill) in [
            ("pending", "pending"),
            ("disabled", "removed"),
            ("error", "error"),
            ("offline", "offline"),
            ("removed", "removed"),
        ] {
            host.status = stored.into();
            host.last_seen_at = stale.clone();
            host.last_error = Some("disk full".into());
            assert_eq!(
                public_status(&host, now_ms),
                stored,
                "{stored} + 91s silent must not become offline"
            );
            assert_eq!(status_label(public_status(&host, now_ms)), pill);
            let copy = card_copy(&host, now_ms);
            assert!(
                copy.contains(pill),
                "{stored} card must keep the {pill} pill: {copy}"
            );
            if stored != "offline" {
                assert!(
                    !copy.split_whitespace().any(|w| w == "offline"),
                    "{stored} must not be overwritten to offline: {copy}"
                );
            }
        }

        // Fresh last_seen must not revive a stored offline / error / pending.
        host.last_seen_at = iso("2026-08-20T12:00:00Z");
        host.status = "offline".into();
        assert_eq!(public_status(&host, now_ms), "offline");
        host.status = "error".into();
        assert_eq!(public_status(&host, now_ms), "error");
        host.status = "pending".into();
        assert_eq!(public_status(&host, now_ms), "pending");
        host.status = "disabled".into();
        assert_eq!(public_status(&host, now_ms), "disabled");
        assert_eq!(status_label(public_status(&host, now_ms)), "removed");
    }

    #[test]
    fn display_name_falls_back_when_blank() {
        let mut host = sample_host();
        assert_eq!(display_name(&host), "box");
        host.name = "  ".into();
        assert_eq!(display_name(&host), "Unnamed worker");
        host.name = String::new();
        assert_eq!(display_name(&host), "Unnamed worker");
    }

    #[test]
    fn display_name_tags_control_plane_and_this_machine() {
        let mut host = sample_host();
        host.is_control_plane_box = true;
        assert_eq!(display_name(&host), "box (control plane)");
        host.is_this_machine = true;
        assert_eq!(display_name(&host), "box (control plane · this machine)");
        host.is_control_plane_box = false;
        assert_eq!(display_name(&host), "box (this machine)");
    }

    #[test]
    fn format_bytes_and_disk_line() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        let mut host = sample_host();
        host.hardware = Some(HostHardware {
            disk_free_bytes: Some(50 * 1024 * 1024 * 1024),
            disk_total_bytes: Some(100 * 1024 * 1024 * 1024),
            ..HostHardware::default()
        });
        assert_eq!(
            disk_free_line(&host).as_deref(),
            Some("50.0 GB free of 100.0 GB")
        );
    }

    #[test]
    fn provider_install_command_picks_os() {
        assert!(provider_install_command("grok", Some("linux-x86_64")).contains("install.sh"));
        assert!(provider_install_command("grok", Some("windows-x86_64")).contains("install.ps1"));
        assert!(provider_install_command("ollama", Some("linux-x86_64")).contains("ollama.com"));
    }

    #[test]
    fn job_cap_is_min_of_plane_and_advertised() {
        let mut host = sample_host();
        host.max_concurrent = 8;
        host.advertised_max_concurrent = None;
        assert_eq!(job_cap(&host), 8);

        host.advertised_max_concurrent = Some(4);
        assert_eq!(job_cap(&host), 4);

        host.max_concurrent = 0;
        host.advertised_max_concurrent = Some(0);
        assert_eq!(job_cap(&host), 1);

        host.max_concurrent = 512;
        host.advertised_max_concurrent = Some(300);
        assert_eq!(job_cap(&host), 256);
    }

    #[test]
    fn not_taking_reason_covers_pair_removed_error_hold() {
        let mut host = sample_host();
        host.status = "pending".into();
        assert_eq!(
            not_taking_reason(&host).as_deref(),
            Some("Waiting to pair.")
        );

        host.status = "disabled".into();
        assert_eq!(
            not_taking_reason(&host).as_deref(),
            Some("Removed — will not take jobs.")
        );

        host.status = "error".into();
        host.last_error = Some("disk full".into());
        assert_eq!(not_taking_reason(&host).as_deref(), Some("disk full"));
        host.last_error = None;
        assert_eq!(
            not_taking_reason(&host).as_deref(),
            Some("Error — not taking jobs.")
        );

        host.status = "online".into();
        host.paused = true;
        host.current_jobs = 0;
        let hold = not_taking_reason(&host).expect("hold");
        assert_no_pause(&hold);
        assert!(hold.contains("Not taking new jobs"), "{hold}");

        host.paused = false;
        host.allow_host = false;
        host.advertised_host = false;
        assert_eq!(
            not_taking_reason(&host).as_deref(),
            Some("Not taking jobs.")
        );
    }

    #[test]
    fn software_title_matches_flag() {
        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());
        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"git_sha":"9d239a509055abcdef","bins":{"fleet-host":{"sha256":"h","version":"0.0.1"},"fleet-driver":{"sha256":"d","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.auto_update = false;
        assert!(
            software_title(&host).contains("linux-x86_64"),
            "{}",
            software_title(&host)
        );

        host.current_versions = PackageVersions {
            host: "0.0.2".into(),
            driver: "0.0.2".into(),
        };
        assert!(
            software_title(&host).contains("fleet-host and/or fleet-driver"),
            "{}",
            software_title(&host)
        );

        host.software = serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef","updating":true}"#)
            .expect("software");
        assert_eq!(
            software_title(&host),
            "Installing fleet-host and fleet-driver from this plane."
        );
    }

    #[test]
    fn served_platforms_omits_bake_only_entries() {
        let inv = PackageInventory {
            packages: vec![
                HostDesiredSoftware {
                    platform: Some("linux-x86_64".into()),
                    git_sha: Some("only-a-stamp".into()),
                    ..HostDesiredSoftware::default()
                },
                HostDesiredSoftware {
                    platform: Some("windows-x86_64".into()),
                    git_sha: Some("only-a-stamp".into()),
                    ..HostDesiredSoftware::default()
                },
            ],
            ..PackageInventory::default()
        };
        assert!(served_platforms(&inv).is_empty());
    }

    #[test]
    fn software_never_uses_file_hash_as_build_id() {
        let mut host = sample_host();
        let host_sha = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e";
        host.software =
            serde_json::from_str(&format!(r#"{{"bins":{{"fleet-host":"{host_sha}"}}}}"#))
                .expect("software");

        // Operator card is software_bin_rows, not software_line.
        let rows = software_bin_rows(&host);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "fleet-host");
        assert_eq!(
            rows[0].version, "—",
            "hash-only report must show em-dash, not the first 12 of the bin SHA"
        );
        assert_eq!(rows[1].name, "fleet-driver");
        assert_eq!(rows[1].version, "—");
        assert_ne!(rows[0].version.as_str(), &host_sha[..12]);
        assert_card_row_versions_are_semver_or_emdash(&host);

        assert_eq!(software_build_id(&host), None, "a bin SHA is not a bake id");
        assert_software_build_id_never_hash_or_bin_sha(&host);

        // 64-hex stuffed into git_sha is still a file hash, not a bake id.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{host_sha}","bins":{{"fleet-host":"{host_sha}"}}}}"#
        ))
        .expect("software");
        assert_eq!(software_build_id(&host), None);
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_eq!(software_bin_rows(&host)[0].version, "—");
        assert_card_row_versions_are_semver_or_emdash(&host);
    }

    /// catalog:list-software-hash-not-bake-id
    /// Heartbeat that only sends sha256 must not treat that file hash as the
    /// bake id, and the card must not display the SHA as a version.
    #[test]
    fn software_hash_is_never_bake_id_or_card_version() {
        let host_sha = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e";
        let driver_sha = "6bec5b00654e94f1a469f1f7cee6f939413b6e36a942b444e798439fb642309d";
        let other_64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let bake = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351";

        fn card_software_lines(host: &HostPublic) -> Vec<String> {
            software_bin_rows(host)
                .into_iter()
                .map(|r| format!("{} {} · {}", r.name, r.version, r.status))
                .collect()
        }

        fn assert_sha256_only_not_on_card(host: &HostPublic, hashes: &[&str]) {
            assert_eq!(
                software_build_id(host),
                None,
                "a file hash is not a bake id on the card"
            );
            assert_software_build_id_never_hash_or_bin_sha(host);
            assert_card_row_versions_are_semver_or_emdash(host);
            let rows = software_bin_rows(host);
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert_eq!(
                    row.version, "—",
                    "sha256-only heartbeat must show em-dash, not the SHA as version ({})",
                    row.name
                );
            }
            for line in card_software_lines(host) {
                for h in hashes {
                    let h = h.trim();
                    let lower_line = line.to_ascii_lowercase();
                    let lower_h = h.to_ascii_lowercase();
                    assert!(
                        !lower_line.contains(&lower_h),
                        "card line must not contain file hash {h}: {line}"
                    );
                    if h.len() >= 12 {
                        assert!(
                            !lower_line.contains(&lower_h[..12]),
                            "card line must not contain short_sha of {h}: {line}"
                        );
                    }
                }
            }
            for surface in [
                software_line(host),
                software_title(host),
                extra_status_pills(host).join(" "),
                identity_line(host).unwrap_or_default(),
            ] {
                for h in hashes {
                    let h = h.trim();
                    assert!(
                        !surface
                            .to_ascii_lowercase()
                            .contains(&h.to_ascii_lowercase()),
                        "card surface must not contain file hash {h}: {surface}"
                    );
                }
            }
        }

        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());

        // Object-form heartbeat: sha256 only — no version, no git_sha.
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha]);

        // String-form hashes only (older host).
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host":"{host_sha}","fleet-driver":"{driver_sha}"}}}}"#
        ))
        .expect("software");
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha]);

        // Windows .exe object-form sha256 only.
        host.platform = Some("windows-x86_64".into());
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host.exe":{{"sha256":"{host_sha}"}},"fleet-driver.exe":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha]);

        // Uppercase / whitespace 64-hex in git_sha is still a file hash.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"  {other_64}  ","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha, other_64]);

        // git_sha copied from a reported bin SHA is still a file hash.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{host_sha}","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha]);

        // desired / plane stamps that are file hashes must not stand in.
        host.desired = Some(HostDesiredSoftware {
            git_sha: Some(host_sha.into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": host_sha},
                "fleet-driver": {"sha256": driver_sha}
            })),
            ..HostDesiredSoftware::default()
        });
        host.plane_git_sha = Some(host_sha.into());
        assert_sha256_only_not_on_card(&host, &[host_sha, driver_sha]);
        assert_ne!(software_build_id(&host), Some(host_sha));
        assert_ne!(software_build_id(&host), Some(driver_sha));

        // Control: a 40-hex tree-id with different 64-hex bin hashes is a bake
        // id internally (plane-bake skip). The card still must not print the
        // file hashes — or that bake id — as a version.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{bake}","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(software_build_id(&host), Some(bake));
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_card_row_versions_are_semver_or_emdash(&host);
        for row in software_bin_rows(&host) {
            assert_eq!(row.version, "—");
        }
        for line in card_software_lines(&host) {
            assert!(
                !line.contains(host_sha) && !line.contains(&host_sha[..12]),
                "card must not print host file hash as version: {line}"
            );
            assert!(
                !line.contains(driver_sha) && !line.contains(&driver_sha[..12]),
                "card must not print driver file hash as version: {line}"
            );
            assert!(
                !line.contains(bake),
                "card must not print bake id as version: {line}"
            );
        }

        // just test excludes fleet-ui; lock the live RSX so interpolating
        // software_build_id / git_sha / sha256 as the version still fails.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let gate = "if shows_software_line(&host)";
        let gate_at = prod.find(gate).expect("if shows_software_line(&host)");
        let after_gate = &prod[gate_at..];
        let block_end = after_gate
            .find("\"{jobs_line(&host)}\"")
            .unwrap_or(after_gate.len());
        let inner = &after_gate[..block_end];
        assert!(
            inner.contains("for row in software_bin_rows(&host)"),
            "package rows come from software_bin_rows, not a hash/bake fallback"
        );
        assert!(
            inner.contains("\"{row.name} {row.version} · {row.status}\""),
            "card renders software_bin_rows version, never a raw SHA"
        );
        for needle in [
            "software_build_id",
            "git_sha",
            "sha256",
            "plane_git_sha",
            "FLEET_GIT_SHA",
        ] {
            assert!(
                !inner.contains(needle),
                "card software rows must not interpolate {needle} as version"
            );
        }
        assert!(
            !prod
                .replace("shows_software_line", "")
                .contains("software_line("),
            "card must not render software_line(); hash-only reports used to leak short_sha there"
        );
    }

    #[test]
    fn software_build_id_is_git_sha_never_bin_hash() {
        let mut host = sample_host();
        assert_eq!(software_build_id(&host), None);

        let host_sha = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e";
        let driver_sha = "6bec5b00654e94f1a469f1f7cee6f939413b6e36a942b444e798439fb642309d";
        let other_64 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let bake = "18a1be7df0a53136e7a85b741d8b9fc45b4cc351";

        // String-form hashes only — integrity, not bake id. Staged bake / plane
        // bake / desired.git_sha must not be used as a stand-in either.
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host":"{host_sha}","fleet-driver":"{driver_sha}"}}}}"#
        ))
        .expect("software");
        host.desired = Some(HostDesiredSoftware {
            git_sha: Some(bake.into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": host_sha},
                "fleet-driver": {"sha256": driver_sha}
            })),
            ..HostDesiredSoftware::default()
        });
        host.plane_git_sha = Some(bake.into());
        assert_eq!(software_build_id(&host), None);
        assert_ne!(software_build_id(&host), Some(host_sha));
        assert_ne!(software_build_id(&host), Some(driver_sha));
        assert_ne!(software_build_id(&host), Some(bake));
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_card_row_versions_are_semver_or_emdash(&host);
        assert_eq!(software_bin_rows(&host)[0].version, "—");
        assert_eq!(software_bin_rows(&host)[1].version, "—");

        // Object-form sha256 + empty / whitespace git_sha still not a hash.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"   ","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(software_build_id(&host), None);
        assert_software_build_id_never_hash_or_bin_sha(&host);

        host.software = serde_json::from_str(r#"{"git_sha":""}"#).expect("software");
        assert_eq!(software_build_id(&host), None);

        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host.exe":{{"sha256":"{host_sha}"}},"fleet-driver.exe":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(software_build_id(&host), None);
        assert_software_build_id_never_hash_or_bin_sha(&host);

        // 64-hex git_sha is never a bake id, even when it is not any reported bin SHA.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{other_64}","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(
            software_build_id(&host),
            None,
            "a 64-hex git_sha is a file hash, not a bake id"
        );
        assert_software_build_id_never_hash_or_bin_sha(&host);

        // Bake id is software.git_sha only, when it is a tree-id and the bins
        // carry different 64-hex hashes. A 40-hex git_sha is a valid bake id
        // — do not disqualify it by stuffing it into sha256.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{bake}","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(software_build_id(&host), Some(bake));
        assert_ne!(software_build_id(&host), Some(host_sha));
        assert_ne!(software_build_id(&host), Some(driver_sha));
        assert_software_build_id_never_hash_or_bin_sha(&host);

        // A git_sha that equals a reported 64-hex bin SHA is a file hash.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{host_sha}","bins":{{"fleet-host":{{"sha256":"{host_sha}"}},"fleet-driver":{{"sha256":"{driver_sha}"}}}}}}"#
        ))
        .expect("software");
        assert_eq!(
            software_build_id(&host),
            None,
            "file hash stuffed into git_sha is still a file hash, not a bake id"
        );
        assert_software_build_id_never_hash_or_bin_sha(&host);
    }

    #[test]
    fn software_status_vs_staged_manifest_not_hash_or_bake() {
        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());
        let host_sha = "aaaaaaaaaaaa1111bbbbbbbb2222ccccdddd3333eeeeffff4444aaaabbbbcccc";
        let driver_sha = "1111aaaaaaaa2222bbbbbbbb3333ccccdddd4444eeeeffff5555aaaabbbbcccc";
        let other_64 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let bake = "9d239a509055abcdef0123456789abcd01234567";

        // No report → unknown, not behind.
        assert_eq!(software_flag(&host), None);
        assert!(!needs_software_update(&host));
        assert_card_row_versions_are_semver_or_emdash(&host);
        assert_eq!(software_build_id(&host), None);

        // Bake ids match but the box has not reported the staged package
        // versions — current/behind is vs the staged manifest, not bake.
        host.software = serde_json::from_str(&format!(r#"{{"git_sha":"{bake}"}}"#)).expect("sw");
        host.software_status = Some("current".into());
        host.desired = Some(HostDesiredSoftware {
            git_sha: Some(bake.into()),
            platform: Some("linux-x86_64".into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": host_sha, "version": "0.0.2"},
                "fleet-driver": {"sha256": driver_sha, "version": "0.0.2"}
            })),
            ..HostDesiredSoftware::default()
        });
        host.current_versions = PackageVersions {
            host: "0.0.2".into(),
            driver: "0.0.2".into(),
        };
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(software_build_id(&host), Some(bake));
        assert_software_build_id_never_hash_or_bin_sha(&host);
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].version, "0.0.2");
        assert_eq!(rows[1].version, "0.0.2");
        assert_eq!(rows[0].status, "missing");
        assert_eq!(rows[1].status, "missing");
        assert_card_row_versions_are_semver_or_emdash(&host);

        // Versions disagree with the staged package. git_sha is a tree-id;
        // bins have different 64-hex hashes. software_build_id stays Some(bake).
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{bake}","bins":{{"fleet-host":{{"sha256":"{host_sha}","version":"0.0.1"}},"fleet-driver":{{"sha256":"{driver_sha}","version":"0.0.1"}}}}}}"#
        ))
        .expect("sw");
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].status, "behind");
        assert_eq!(rows[1].status, "behind");
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(
            software_build_id(&host),
            Some(bake),
            "a 40-hex tree-id is a valid bake id when bins carry different 64-hex hashes"
        );
        assert_eq!(rows[0].version, "0.0.1");
        assert_eq!(rows[1].version, "0.0.1");
        assert_card_row_versions_are_semver_or_emdash(&host);
        assert_software_build_id_never_hash_or_bin_sha(&host);

        // 64-hex git_sha → None, even when it is not a reported bin SHA.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{other_64}","bins":{{"fleet-host":{{"sha256":"{host_sha}","version":"0.0.1"}},"fleet-driver":{{"sha256":"{driver_sha}","version":"0.0.1"}}}}}}"#
        ))
        .expect("sw");
        assert_eq!(software_build_id(&host), None);
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_card_row_versions_are_semver_or_emdash(&host);

        // git_sha that equals a reported 64-hex bin SHA → None.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"{host_sha}","bins":{{"fleet-host":{{"sha256":"{host_sha}","version":"0.0.1"}},"fleet-driver":{{"sha256":"{driver_sha}","version":"0.0.1"}}}}}}"#
        ))
        .expect("sw");
        assert_eq!(software_build_id(&host), None);
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_eq!(software_bin_rows(&host)[0].version, "0.0.1");
        assert_card_row_versions_are_semver_or_emdash(&host);

        // Staged versions match even when bake / plane / file hashes differ.
        host.software = serde_json::from_str(&format!(
            r#"{{"git_sha":"old-tree-id","bins":{{"fleet-host":{{"sha256":"cargo-host","version":"0.0.2"}},"fleet-driver":{{"sha256":"cargo-drv","version":"0.0.2"}}}}}}"#
        ))
        .expect("sw");
        host.plane_git_sha = Some("other-plane-bake".into());
        host.software_status = Some("behind".into());
        host.outdated = true;
        assert_eq!(software_flag(&host), Some("up to date"));
        assert_eq!(software_build_id(&host), Some("old-tree-id"));
        assert_ne!(software_build_id(&host), Some("cargo-host"));
        assert_software_build_id_never_hash_or_bin_sha(&host);
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].version, "0.0.2");
        assert_eq!(rows[1].version, "0.0.2");
        assert_card_row_versions_are_semver_or_emdash(&host);

        // Hash-only report (old host) vs staged hashes — integrity, not bake.
        // Matching files can be current; the hash still must not become bake id
        // or the operator-visible version.
        host.software = serde_json::from_str(&format!(
            r#"{{"bins":{{"fleet-host":"{host_sha}","fleet-driver":"{driver_sha}"}}}}"#
        ))
        .expect("sw");
        host.desired = Some(HostDesiredSoftware {
            git_sha: Some(host_sha.into()),
            bins: Some(serde_json::json!({
                "fleet-host": {"sha256": host_sha},
                "fleet-driver": {"sha256": driver_sha}
            })),
            ..HostDesiredSoftware::default()
        });
        host.current_versions = PackageVersions::default();
        host.software_status = None;
        host.outdated = false;
        let rows = software_bin_rows(&host);
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[1].status, "current");
        assert_eq!(
            rows[0].version, "—",
            "hash-only integrity must not print a SHA prefix as version"
        );
        assert_eq!(rows[1].version, "—");
        assert_eq!(software_build_id(&host), None);
        assert_ne!(software_build_id(&host), Some(host_sha));
        assert_software_build_id_never_hash_or_bin_sha(&host);
        assert_card_row_versions_are_semver_or_emdash(&host);
    }

    /// catalog:list-relative-seen-just-now-never
    /// catalog: last seen relative — None → never; recent heartbeat → just now;
    /// older stamps use bounded s/m/h/d units. Far-future must not panic.
    fn is_bounded_relative_seen(s: &str) -> bool {
        if s == "never" || s == "just now" {
            return true;
        }
        let Some(rest) = s.strip_suffix(" ago") else {
            return false;
        };
        let mut chars = rest.chars();
        let Some(unit) = chars.next_back() else {
            return false;
        };
        let n: String = chars.collect();
        matches!(unit, 's' | 'm' | 'h' | 'd')
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.as_str() != "0"
            && !n.starts_with('0')
    }

    fn relative_seen_no_panic(ts: Option<String>, now_ms: f64, why: &str) -> String {
        std::panic::catch_unwind(|| relative_seen(&ts, now_ms))
            .unwrap_or_else(|_| panic!("relative_seen must not panic on {why}"))
    }

    #[test]
    fn relative_seen_none_is_never() {
        assert_eq!(relative_seen(&None, 0.0), "never");
        assert_eq!(
            relative_seen(&Some(String::new()), 1_700_000_000_000.0),
            "never"
        );
    }

    #[test]
    fn relative_seen_recent_is_just_now() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:02Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        assert_eq!(
            relative_seen(&Some("2026-08-20T12:00:00Z".into()), now_ms),
            "just now"
        );
        assert_eq!(
            relative_seen(&Some("2026-08-20T11:59:00Z".into()), now_ms),
            "1m ago"
        );
    }

    /// catalog:list-relative-seen-just-now-never
    /// last seen relative: None → never. Recent heartbeat → just now.
    /// Older valid stamps are only Ns/m/h/d ago (no weeks/months/years).
    /// Far-future / clock-skew stamps must not panic or print a negative unit.
    #[test]
    fn relative_seen_is_just_now_never_bounded_units() {
        // just test excludes fleet-ui; lock the live chip so swapping
        // last_seen_label for format_ts / raw last_seen_at cannot go green.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("last_seen_label"),
            "production source must import last_seen_label"
        );
        assert!(
            !prod.contains("fn last_seen_label") && !prod.contains("fn relative_seen"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert_eq!(
            prod.matches("last_seen_label(&host, now_ms)").count(),
            2,
            "list row + detail must both call last_seen_label"
        );
        assert_eq!(
            prod.matches("last seen {last_seen}").count(),
            2,
            "host cards must render last seen {{relative_seen}}, not a raw stamp"
        );

        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        let seen = |raw: &str| relative_seen_no_panic(Some(raw.to_string()), now_ms, raw);

        // Trap: a helper that printed "—" / "unknown" / raw ISO would pass a
        // non-empty check. Catalog last-seen is the word "never".
        assert_eq!(relative_seen(&None, now_ms), "never");
        assert_eq!(relative_seen(&Some(String::new()), now_ms), "never");
        assert!(is_bounded_relative_seen("never"));

        // Recent heartbeat (a few seconds of jitter) is "just now", not "0s ago".
        assert_eq!(seen("2026-08-20T12:00:00Z"), "just now");
        assert_eq!(seen("2026-08-20T11:59:59Z"), "just now");
        assert_eq!(seen("2026-08-20T11:59:56Z"), "just now");
        assert_eq!(seen("2026-08-20T12:00:00.000Z"), "just now");
        assert_eq!(seen("2026-08-20T12:00:00+00:00"), "just now");
        assert_eq!(seen("2026-08-20T05:00:00-07:00"), "just now");

        // Bounded units only: s / m / h / d. Never weeks, months, years,
        // signed deltas, or the raw ISO on a parseable stamp.
        assert_eq!(seen("2026-08-20T11:59:55Z"), "5s ago");
        assert_eq!(seen("2026-08-20T11:59:01Z"), "59s ago");
        assert_eq!(seen("2026-08-20T11:59:00Z"), "1m ago");
        assert_eq!(seen("2026-08-20T11:00:01Z"), "59m ago");
        assert_eq!(seen("2026-08-20T11:00:00Z"), "1h ago");
        assert_eq!(seen("2026-08-20T10:00:00Z"), "2h ago");
        assert_eq!(seen("2026-08-19T12:00:01Z"), "23h ago");
        assert_eq!(seen("2026-08-19T12:00:00Z"), "1d ago");
        assert_eq!(seen("2026-08-17T12:00:00Z"), "3d ago");
        let epoch_days = now.timestamp() / 86_400;
        assert_eq!(
            seen("1970-01-01T00:00:00Z"),
            format!("{epoch_days}d ago"),
            "old stamps stay in days, not years/months"
        );
        for stamp in [
            "2026-08-20T11:59:55Z",
            "2026-08-20T11:59:00Z",
            "2026-08-20T10:00:00Z",
            "2026-08-17T12:00:00Z",
            "1970-01-01T00:00:00Z",
        ] {
            let got = seen(stamp);
            assert!(
                is_bounded_relative_seen(&got),
                "parseable last_seen must be just now / never / Ns|m|h|d ago, got {got:?} ({stamp})"
            );
            assert!(
                !got.contains("week")
                    && !got.contains("month")
                    && !got.contains("year")
                    && !got.contains('-')
                    && !got.contains('T'),
                "must not leak an unbounded unit or raw stamp: {got:?} ({stamp})"
            );
        }

        // Compact (no Z) still parses as a heartbeat, not a raw leftover.
        assert_eq!(seen("2026-08-20T12:00:00"), "just now");
        assert_eq!(seen("2026-08-20T11:59:50"), "10s ago");

        // Clock skew / far-future: clamp to just now. Must not panic, must
        // not print "-5s ago" / a future unit / a truncated ISO.
        for stamp in [
            "2026-08-20T12:00:01Z",
            "2026-08-20T13:00:00Z",
            "2027-08-20T12:00:00Z",
            "9999-12-31T23:59:59Z",
            "9999-12-31T23:59:59",
        ] {
            let got = seen(stamp);
            assert_eq!(
                got, "just now",
                "far-future last_seen must clamp to just now, not panic or a signed unit: {got:?} ({stamp})"
            );
        }

        // Unparseable 5+ digit years must not panic (chrono RFC3339 max is
        // 9999). Fail-soft; do not abort the list render.
        let _ = relative_seen_no_panic(
            Some("+292277026596-01-01T00:00:00Z".into()),
            now_ms,
            "overflow-year RFC3339",
        );
        let _ = relative_seen_no_panic(
            Some("275760-09-13T00:00:00Z".into()),
            now_ms,
            "js Date max year",
        );

        // last_seen_label is what the card renders. Hosts use relative_seen;
        // the plane is "this process", not never/just now.
        let mut host = sample_host();
        host.kind = "host".into();
        host.last_seen_at = None;
        assert_eq!(last_seen_label(&host, now_ms), "never");
        assert!(
            card_copy(&host, now_ms).contains("never"),
            "{}",
            card_copy(&host, now_ms)
        );

        host.last_seen_at = Some("2026-08-20T12:00:00Z".into());
        assert_eq!(last_seen_label(&host, now_ms), "just now");
        assert_eq!(
            last_seen_label(&host, now_ms),
            relative_seen(&host.last_seen_at, now_ms)
        );
        assert!(
            card_copy(&host, now_ms).contains("just now"),
            "{}",
            card_copy(&host, now_ms)
        );

        host.last_seen_at = Some("9999-12-31T23:59:59Z".into());
        assert_eq!(
            std::panic::catch_unwind(|| last_seen_label(&host, now_ms))
                .unwrap_or_else(|_| panic!("last_seen_label must not panic on year 9999")),
            "just now"
        );

        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.last_seen_at = None;
        assert_eq!(last_seen_label(&plane, now_ms), "this process");
        assert_ne!(
            last_seen_label(&plane, now_ms),
            relative_seen(&None, now_ms)
        );
    }

    #[test]
    fn worker_id_label_prefers_machine_id_hex() {
        let mut host = sample_host();
        host.machine_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());
        assert_eq!(worker_id_label(&host), "aaaaaaaa");

        host.machine_id = None;
        host.id = "0123456789abcdef".into();
        assert_eq!(worker_id_label(&host), "01234567");

        host.id = "short".into();
        assert_eq!(worker_id_label(&host), "short");
    }

    /// catalog:list-worker-id-label-8hex — Stable 8-hex id from `machine_id`
    /// (else host id). Regex `id [0-9a-f]{8}`. Same machine_id after
    /// revoke+reclaim keeps the label. Missing machine_id falls back to
    /// the host id prefix, not empty.
    #[test]
    fn worker_id_label_is_stable_8hex_from_machine_id_else_host_id() {
        // just test excludes fleet-ui; lock the live RSX so interpolating
        // host.id (or dropping the id span) still fails. Catalog e2e:
        // Regex `id [0-9a-f]{8}`.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("worker_id_label"),
            "production source must import worker_id_label"
        );
        assert!(
            !prod.contains("fn worker_id_label"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert!(
            prod.matches("id {worker_id_label(&host)}").count() >= 2,
            "list card and detail panel must render id {{worker_id_label(&host)}}"
        );
        assert!(
            !prod.contains("id {host.id}") && !prod.contains("id {host.machine_id"),
            "must not interpolate host.id / machine_id raw (must be the 8-hex helper)"
        );

        fn is_8_hex(s: &str) -> bool {
            s.len() == 8 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        }
        fn assert_card_id(host: &HostPublic, want: &str) {
            let label = worker_id_label(host);
            assert_eq!(label, want, "worker_id_label({:?})", host.machine_id);
            assert!(
                is_8_hex(&label),
                "worker_id_label must match [0-9a-f]{{8}}, got {label:?}"
            );
            let rendered = format!("id {label}");
            assert!(
                rendered == format!("id {want}") && is_8_hex(&rendered[3..]),
                "card must render id [0-9a-f]{{8}}, got {rendered:?}"
            );
            assert!(!label.is_empty(), "worker_id_label must not be empty");
        }

        // machine_id UUID: first 8 hex digits. Not the host row id.
        let mut live = sample_host();
        live.id = "ffffffff01234567".into();
        live.kind = "host".into();
        live.status = "online".into();
        live.machine_id = Some("cafebabe-0123-4567-89ab-cdef01234567".into());
        live.paired_at = Some("2026-01-01T00:00:00Z".into());
        assert_card_id(&live, "cafebabe");
        assert_ne!(
            worker_id_label(&live),
            "ffffffff",
            "machine_id must win over a different host id"
        );

        // Surrounding whitespace is still that machine_id.
        live.machine_id = Some("  cafebabe-0123-4567-89ab-cdef01234567  ".into());
        assert_card_id(&live, "cafebabe");

        // Uppercase UUID still renders lowercase 8-hex (e2e regex is [0-9a-f]).
        live.machine_id = Some("CAFEBABE-0123-4567-89AB-CDEF01234567".into());
        assert_card_id(&live, "cafebabe");

        // Revoke then reclaim the same guest: new host row id, same
        // durable machine_id → same 8-hex label.
        let mut revoked = live.clone();
        revoked.status = "disabled".into();
        revoked.machine_id = Some("cafebabe-0123-4567-89ab-cdef01234567".into());
        assert_card_id(&revoked, "cafebabe");
        assert_eq!(
            worker_id_label(&revoked),
            worker_id_label(&live),
            "revoke must not change the 8-hex label"
        );

        let mut reclaimed = live.clone();
        reclaimed.id = "0000000011111111".into();
        reclaimed.status = "online".into();
        reclaimed.machine_id = Some("cafebabe-0123-4567-89ab-cdef01234567".into());
        assert_ne!(reclaimed.id, live.id);
        assert_card_id(&reclaimed, "cafebabe");
        assert_eq!(
            worker_id_label(&reclaimed),
            worker_id_label(&revoked),
            "same machine_id after revoke+reclaim must keep the 8-hex label"
        );
        assert_ne!(
            worker_id_label(&reclaimed),
            "00000000",
            "reclaim must not snap the label to the new host id"
        );

        // Missing machine_id falls back to host id prefix, not empty.
        let mut unpaired = sample_host();
        unpaired.kind = "host".into();
        unpaired.status = "pending".into();
        unpaired.id = "0123456789abcdef".into();
        unpaired.machine_id = None;
        assert_card_id(&unpaired, "01234567");
        assert!(!worker_id_label(&unpaired).is_empty());

        unpaired.machine_id = Some(String::new());
        assert_card_id(&unpaired, "01234567");
        unpaired.machine_id = Some("   ".into());
        assert_card_id(&unpaired, "01234567");

        // Host-id fallback still prefers hex digits (UUID dashes dropped).
        unpaired.id = "deadbeef-0123-4567-89ab-cdef01234567".into();
        unpaired.machine_id = None;
        assert_card_id(&unpaired, "deadbeef");
    }

    #[test]
    fn sort_hosts_plane_then_status_then_name() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        fn host(id: &str, kind: &str, status: &str, name: &str) -> HostPublic {
            let mut h = sample_host();
            h.id = id.into();
            h.kind = kind.into();
            h.status = status.into();
            h.name = name.into();
            if status == "online" {
                h.last_seen_at = Some("2026-08-20T12:00:00Z".into());
            }
            h
        }
        let plane = host("plane", "control_plane", "offline", "zzz-plane");
        let online_z = host("oz", "host", "online", "zebra");
        let online_a = host("oa", "host", "online", "apple");
        let pending = host("p", "host", "pending", "mid");
        let error = host("e", "host", "error", "err");
        let offline = host("off", "host", "offline", "off");
        let unknown = host("u", "host", "mystery", "zzz");
        let mut active = vec![
            offline.clone(),
            unknown.clone(),
            online_z.clone(),
            plane.clone(),
            error.clone(),
            pending.clone(),
            online_a.clone(),
        ];
        active.sort_by(|a, b| cmp_hosts_for_list(a, b, now_ms));
        let ids: Vec<&str> = active.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["plane", "oa", "oz", "p", "e", "off", "u"]);
        assert_eq!(status_rank("online"), 0);
        assert_eq!(status_rank("pending"), 1);
        assert_eq!(status_rank("error"), 2);
        assert_eq!(status_rank("offline"), 3);
        assert_eq!(status_rank("disabled"), 4);
        assert_eq!(status_rank("mystery"), 5);
        assert!(cmp_hosts_for_list(&plane, &online_a, now_ms).is_lt());
        assert!(cmp_hosts_for_list(&online_a, &online_z, now_ms).is_lt());
        assert!(cmp_hosts_for_list(&online_z, &pending, now_ms).is_lt());
    }

    /// catalog:list-sort-control-plane-first — Offline named "aaa" must not
    /// beat the plane card. Pending vs online is status then name.
    #[test]
    fn sort_control_plane_first_then_status_then_name() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        fn host(id: &str, kind: &str, status: &str, name: &str) -> HostPublic {
            let mut h = sample_host();
            h.id = id.into();
            h.kind = kind.into();
            h.status = status.into();
            h.name = name.into();
            if status == "online" {
                h.last_seen_at = Some("2026-08-20T12:00:00Z".into());
            }
            h
        }

        // Name-first or status-then-name (no plane key) would put "aaa"
        // first: both offline, plane name is last alphabetically.
        let plane = host("plane", "control_plane", "offline", "zzz-plane");
        let aaa = host("aaa", "host", "offline", "aaa");
        assert!(is_control_plane(&plane));
        assert!(!is_control_plane(&aaa));
        assert!(
            cmp_hosts_for_list(&plane, &aaa, now_ms).is_lt(),
            "offline named aaa must not beat the plane card"
        );
        assert!(
            cmp_hosts_for_list(&aaa, &plane, now_ms).is_gt(),
            "offline named aaa must sort after the plane card"
        );

        // Status then name: pending "aaa" must not beat online "zzz".
        let pending_aaa = host("pend", "host", "pending", "aaa");
        let online_zzz = host("on", "host", "online", "zzz");
        assert!(
            cmp_hosts_for_list(&online_zzz, &pending_aaa, now_ms).is_lt(),
            "pending vs online is status then name"
        );
        assert!(cmp_hosts_for_list(&pending_aaa, &online_zzz, now_ms).is_gt());

        // Name is the last key among the same status.
        let online_apple = host("oa", "host", "online", "apple");
        assert!(cmp_hosts_for_list(&online_apple, &online_zzz, now_ms).is_lt());

        // Stored online + last_seen older than 90s is the offline pill — sort
        // with that rank, not the stored online bucket.
        let mut silent_aaa = host("silent", "host", "online", "aaa");
        silent_aaa.last_seen_at = Some("2026-08-20T11:58:29Z".into());
        assert_eq!(public_status(&silent_aaa, now_ms), "offline");
        assert!(
            cmp_hosts_for_list(&pending_aaa, &silent_aaa, now_ms).is_lt(),
            "silent stored-online must sort as offline, after pending"
        );
        assert!(cmp_hosts_for_list(&silent_aaa, &online_zzz, now_ms).is_gt());

        let mut active = vec![
            aaa.clone(),
            pending_aaa.clone(),
            online_zzz.clone(),
            plane.clone(),
            online_apple.clone(),
            silent_aaa.clone(),
        ];
        active.sort_by(|a, b| cmp_hosts_for_list(a, b, now_ms));
        let ids: Vec<&str> = active.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["plane", "oa", "on", "pend", "aaa", "silent"]);
    }

    #[test]
    fn shows_software_line_only_on_paired_boxes() {
        let mut host = sample_host();
        host.status = "online".into();
        assert!(shows_software_line(&host));

        host.status = "offline".into();
        assert!(shows_software_line(&host));

        host.status = "error".into();
        assert!(shows_software_line(&host));

        host.status = "pending".into();
        assert!(!shows_software_line(&host));

        host.status = "disabled".into();
        assert!(!shows_software_line(&host));

        host.status = "online".into();
        host.kind = "control_plane".into();
        assert!(!shows_software_line(&host));
    }

    /// catalog:list-plane-card-no-software-line — Control plane card has no
    /// software / package rows. `as_public` software is none; the card gate
    /// is `shows_software_line` (false for kind=control_plane).
    #[test]
    fn control_plane_card_has_no_software_or_package_rows() {
        // Plane as_public is this process, not a host package. Lock the
        // struct literal so attaching software / desired still fails here.
        let plane_src = include_str!("../../fleet-db/src/models/plane_runner.rs");
        let plane_prod = plane_src
            .split("#[cfg(test)]")
            .next()
            .expect("plane_runner.rs production source");
        let as_public = {
            let start = plane_prod
                .find("fn as_public_with_jobs")
                .expect("as_public_with_jobs");
            let rest = &plane_prod[start..];
            let end = rest.find("fn local_platform").unwrap_or(rest.len());
            &rest[..end]
        };
        assert!(
            as_public.contains("software: None,"),
            "as_public software must be none"
        );
        assert!(
            as_public.contains("desired: None,"),
            "as_public desired package must be none"
        );
        assert!(
            !as_public.contains("software: Some") && !as_public.contains("desired: Some"),
            "plane as_public must not attach software or a served package"
        );
        assert!(
            as_public.contains("kind: WORKER_HOST_KIND_CONTROL_PLANE.to_string()"),
            "as_public kind must be control_plane"
        );
        assert!(
            plane_prod.contains("self.as_public_with_jobs(now, 0)"),
            "as_public must reuse as_public_with_jobs (same software: None)"
        );

        // just test excludes fleet-ui; lock the live RSX so rendering
        // software_bin_rows without shows_software_line still fails.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("shows_software_line"),
            "production source must import shows_software_line"
        );
        assert!(
            import.contains("software_bin_rows"),
            "production source must import software_bin_rows"
        );
        assert!(
            !prod.contains("fn shows_software_line") && !prod.contains("fn software_bin_rows"),
            "page.rs must call the crate helpers, not a local shadow"
        );
        assert!(
            !prod
                .replace("shows_software_line", "")
                .contains("software_line("),
            "card must not render software_line(); package rows come from software_bin_rows"
        );
        let gate = "if shows_software_line(&host)";
        assert_eq!(
            prod.matches(gate).count(),
            1,
            "production RSX must gate package rows with if shows_software_line(&host) exactly once"
        );
        let gate_at = prod.find(gate).expect("if shows_software_line(&host)");
        let after_gate = &prod[gate_at..];
        let block_end = after_gate
            .find("\"{jobs_line(&host)}\"")
            .unwrap_or(after_gate.len());
        let inner = &after_gate[..block_end];
        assert!(
            inner.contains("for row in software_bin_rows(&host)"),
            "software_bin_rows must be inside if shows_software_line(&host)"
        );
        assert!(
            inner.contains("\"{row.name} {row.version} · {row.status}\""),
            "package rows must be inside if shows_software_line(&host)"
        );
        assert!(
            !inner.contains("if !plane"),
            "if !plane must not wrap the package rows inside the helper gate"
        );
        let last_if = prod[..gate_at + gate.len()]
            .rfind("if ")
            .expect("if-gate before software rows");
        let gate_line = prod[last_if..].lines().next().unwrap_or("");
        assert_eq!(
            last_if, gate_at,
            "nearest if before software_bin_rows must be if shows_software_line(&host), got {gate_line:?}"
        );
        assert!(
            !gate_line.contains("!plane"),
            "if !plane must not be the only software-row gate"
        );
        assert_eq!(
            prod.matches("software_bin_rows(&host)").count(),
            1,
            "software_bin_rows must appear only inside the shows_software_line gate"
        );

        // Wire payload matches PlaneRunner::as_public: software none.
        let from_api: HostPublic = serde_json::from_str(
            r#"{
                "id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "workspace_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "name":"Control plane",
                "kind":"control_plane",
                "status":"online",
                "software":null,
                "desired":null,
                "software_status":"unknown",
                "outdated":false
            }"#,
        )
        .expect("plane as_public json");
        assert!(
            from_api.software.is_none(),
            "as_public software = none: {from_api:?}"
        );
        assert!(from_api.desired.is_none(), "as_public desired = none");
        assert_eq!(from_api.kind, "control_plane");
        assert!(is_control_plane(&from_api));
        assert!(
            !shows_software_line(&from_api),
            "shows_software_line must be false for kind=control_plane"
        );
        let hidden = software_bin_rows(&from_api);
        assert_eq!(
            hidden.len(),
            PACKAGE_BIN_NAMES.len(),
            "software_bin_rows still emits fleet-host / fleet-driver; the gate hides them"
        );
        assert!(
            hidden.iter().any(|r| r.name == "fleet-host")
                && hidden.iter().any(|r| r.name == "fleet-driver"),
            "ungated card would show package rows: {hidden:?}"
        );

        // kind=control_plane is the gate — not is_control_plane_box, not a
        // stuffed software/desired payload, not status/outdated flags.
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.software = None;
        plane.desired = None;
        plane.platform = Some("linux-x86_64".into());
        plane.outdated = true;
        plane.software_status = Some("behind".into());
        plane.is_control_plane_box = false;
        assert!(is_control_plane(&plane));
        assert!(plane.software.is_none());
        assert!(
            !shows_software_line(&plane),
            "widget must not render on kind=control_plane"
        );

        set_package_versions(&mut plane, "0.0.1", "0.0.2");
        assert!(
            plane.software.is_some() && plane.desired.is_some(),
            "stuffed payload would produce package rows if the gate were dropped"
        );
        assert_eq!(software_flag(&plane), Some("outdated"));
        assert_eq!(software_bin_rows(&plane).len(), PACKAGE_BIN_NAMES.len());
        assert!(
            !shows_software_line(&plane),
            "kind=control_plane hides package rows even when software is present"
        );

        for status in ["online", "offline", "pending", "disabled", "error"] {
            plane.status = status.into();
            assert!(
                !shows_software_line(&plane),
                "kind=control_plane hides software for status={status}"
            );
        }

        plane.status = "online".into();
        plane.is_control_plane_box = true;
        plane.outdated = false;
        plane.software_status = None;
        plane.software = None;
        plane.desired = None;
        assert!(
            !shows_software_line(&plane),
            "kind=control_plane stays hidden even as the plane machine"
        );

        // A paired box tagged as the plane machine still reports host packages.
        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.is_control_plane_box = true;
        set_package_versions(&mut host, "0.0.2", "0.0.2");
        assert!(
            shows_software_line(&host),
            "kind=host still shows package rows even on the plane machine"
        );
        assert_eq!(software_bin_rows(&host).len(), PACKAGE_BIN_NAMES.len());
    }

    /// catalog:unit-auto-update-on-paired — Auto-update checkbox is paired-only.
    /// "Paired only" means not the plane (this process, no host package pull).
    #[test]
    fn auto_update_checkbox_absent_on_control_plane() {
        // just test excludes fleet-ui; lock the live RSX from this host crate
        // so deleting the helper or gating Auto-update with only `if !plane`
        // fails on the default unit path.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("shows_auto_update"),
            "production source must import shows_auto_update"
        );
        assert!(
            !prod.contains("fn shows_auto_update"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert_eq!(
            prod.matches("\"Auto-update\"").count(),
            1,
            "production RSX must render Auto-update exactly once"
        );
        let label_at = prod
            .find("\"Auto-update\"")
            .expect("Auto-update label in production RSX");
        let before = &prod[..label_at];
        let gate_at = before
            .rfind("if shows_auto_update(&host)")
            .expect("if shows_auto_update(&host) before Auto-update");
        let last_if = before.rfind("if ").expect("if-gate before Auto-update");
        let gate_line = before[last_if..].lines().next().unwrap_or("");
        assert_eq!(
            last_if, gate_at,
            "nearest if before Auto-update must be if shows_auto_update(&host), got {gate_line:?}"
        );
        assert!(
            !gate_line.contains("!plane"),
            "if !plane must not be the only Auto-update gate"
        );
        let inner = &prod[gate_at..label_at];
        assert!(
            inner.contains("r#type: \"checkbox\""),
            "Auto-update checkbox must be inside if shows_auto_update(&host)"
        );
        assert!(
            !inner.contains("if !plane"),
            "if !plane must not wrap the Auto-update checkbox inside the helper gate"
        );

        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.auto_update = false;
        assert!(
            shows_auto_update(&host),
            "paired host shows the auto-update checkbox"
        );
        assert_eq!(update_now_label(&host, false), Some("Update now"));

        host.status = "offline".into();
        assert!(shows_auto_update(&host));
        host.status = "error".into();
        assert!(shows_auto_update(&host));
        host.status = "pending".into();
        assert!(
            shows_auto_update(&host),
            "pending pair still shows Auto-update (test_17 / widgets-lock e2e)"
        );

        host.status = "disabled".into();
        assert!(
            !shows_auto_update(&host),
            "removed card has no auto-update widget"
        );

        // kind=control_plane is the gate — not is_control_plane_box, not
        // auto_update/outdated/request flags. Plane is this process.
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.auto_update = true;
        plane.outdated = true;
        plane.software_status = Some("behind".into());
        plane.update_requested_at = Some("requested".into());
        plane.is_control_plane_box = false;
        assert!(is_control_plane(&plane));
        assert!(
            !shows_auto_update(&plane),
            "widget must not render on kind=control_plane"
        );
        assert_eq!(update_now_label(&plane, false), None);
        assert_eq!(update_now_label(&plane, true), None);
        assert!(
            !settings_draft_dirty(&plane, accepting_jobs(&plane), false, plane.max_concurrent),
            "absent widget cannot dirty plane settings"
        );

        plane.status = "offline".into();
        assert!(!shows_auto_update(&plane));
        plane.status = "pending".into();
        assert!(!shows_auto_update(&plane));
        plane.status = "disabled".into();
        assert!(!shows_auto_update(&plane));

        // Flags that might look like "this box should update" still hide
        // the widget on the plane.
        plane.status = "online".into();
        plane.auto_update = false;
        plane.outdated = false;
        plane.software_status = None;
        plane.update_requested_at = None;
        plane.is_control_plane_box = true;
        assert!(
            !shows_auto_update(&plane),
            "kind=control_plane stays hidden even as the plane machine"
        );

        // A paired box tagged as the plane machine still pulls a host package.
        host.kind = "host".into();
        host.status = "online".into();
        host.is_control_plane_box = true;
        assert!(
            shows_auto_update(&host),
            "kind=host still shows auto-update even on the plane machine"
        );
    }

    #[test]
    fn jobs_line_is_cap_plus_load() {
        let mut host = sample_host();
        host.current_jobs = 0;
        host.max_concurrent = 4;
        host.advertised_max_concurrent = None;
        assert_eq!(jobs_line(&host), "jobs 0 / 4");

        host.current_jobs = 2;
        host.advertised_max_concurrent = Some(8);
        host.hardware = Some(HostHardware {
            cpu_used_pct: Some(12),
            mem_used_pct: Some(32),
            gpu_used_pct: Some(8),
            ..HostHardware::default()
        });
        assert_eq!(jobs_line(&host), "jobs 2 / 4 · cpu 12% · mem 32% · gpu 8%");

        host.hardware = Some(HostHardware {
            cpu_used_pct: Some(12),
            mem_used_pct: Some(32),
            ..HostHardware::default()
        });
        assert_eq!(jobs_line(&host), "jobs 2 / 4 · cpu 12% · mem 32%");
        assert!(!jobs_line(&host).contains("gpu"));
    }

    /// catalog:list-jobs-n-cap-min-advertised — Jobs `n / cap` is
    /// min(plane, advertised). Cap display and assignable skip at that min.
    /// 0 advertised is not unlimited.
    #[test]
    fn jobs_n_cap_uses_min_of_plane_and_advertised() {
        // just test excludes fleet-ui; lock the live RSX so interpolating
        // plane max_concurrent (or advertised alone) as the cap still fails.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("jobs_line"),
            "production source must import jobs_line"
        );
        assert!(
            !prod.contains("fn jobs_line") && !prod.contains("fn job_cap"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert!(
            prod.matches("{jobs_line(&host)}").count() >= 2,
            "list card and detail panel must render jobs_line(&host)"
        );
        assert!(
            !prod.contains("jobs {host.current_jobs} / {host.max_concurrent}"),
            "must not show plane max_concurrent as the cap (ignores advertised)"
        );
        assert!(
            !prod.contains("jobs {host.current_jobs} / {host.advertised_max_concurrent"),
            "must not show advertised alone as the cap (ignores plane)"
        );

        let mut host = sample_host();
        host.hardware = None;
        host.kind = "host".into();

        // New card: no advertised report → plane cap. Catalog e2e: jobs 0 / 4.
        host.current_jobs = 0;
        host.max_concurrent = 4;
        host.advertised_max_concurrent = None;
        assert_eq!(job_cap(&host), 4, "no advertised report → plane cap");
        assert_eq!(jobs_line(&host), "jobs 0 / 4");
        assert!(!at_job_cap(&host), "0 jobs is under cap / assignable");

        // Advertised 2 wins over plane 8.
        host.max_concurrent = 8;
        host.advertised_max_concurrent = Some(2);
        host.current_jobs = 0;
        assert_eq!(job_cap(&host), 2, "advertised 2 must win over plane 8");
        assert_eq!(jobs_line(&host), "jobs 0 / 2");
        assert_ne!(
            jobs_line(&host),
            "jobs 0 / 8",
            "must not display the plane cap when advertised is lower"
        );
        assert!(!at_job_cap(&host));

        host.current_jobs = 1;
        assert_eq!(jobs_line(&host), "jobs 1 / 2");
        assert!(!at_job_cap(&host), "1 < min(8,2) stays assignable");

        host.current_jobs = 2;
        assert_eq!(jobs_line(&host), "jobs 2 / 2");
        assert!(
            at_job_cap(&host),
            "current_jobs == advertised min must skip assignable"
        );

        // Plane 2 wins over advertised 8.
        host.max_concurrent = 2;
        host.advertised_max_concurrent = Some(8);
        host.current_jobs = 1;
        assert_eq!(job_cap(&host), 2, "plane 2 must win over advertised 8");
        assert_eq!(jobs_line(&host), "jobs 1 / 2");
        assert_ne!(
            jobs_line(&host),
            "jobs 1 / 8",
            "must not display advertised when the plane cap is lower"
        );
        assert!(!at_job_cap(&host));

        host.current_jobs = 2;
        assert_eq!(jobs_line(&host), "jobs 2 / 2");
        assert!(
            at_job_cap(&host),
            "min(plane=2, advertised=8) at 2 must skip assignable"
        );

        // 0 advertised is not unlimited — clamp 1, first job ok, second skips.
        host.max_concurrent = 8;
        host.advertised_max_concurrent = Some(0);
        host.current_jobs = 0;
        assert_eq!(
            job_cap(&host),
            1,
            "0 advertised is a cap of 1, not unlimited"
        );
        assert_eq!(jobs_line(&host), "jobs 0 / 1");
        assert_ne!(
            jobs_line(&host),
            "jobs 0 / 8",
            "advertised 0 must not fall through to the plane cap"
        );
        assert!(
            !jobs_line(&host).to_ascii_lowercase().contains("unlimited"),
            "{}",
            jobs_line(&host)
        );
        assert!(
            !at_job_cap(&host),
            "0 jobs against advertised-0 still assignable"
        );

        host.current_jobs = 1;
        assert_eq!(jobs_line(&host), "jobs 1 / 1");
        assert!(
            at_job_cap(&host),
            "advertised 0 is not unlimited; 1 job must skip assignable"
        );

        // Plane 0 + advertised 0 still clamp to 1 (not 0 / unlimited).
        host.max_concurrent = 0;
        host.advertised_max_concurrent = Some(0);
        host.current_jobs = 0;
        assert_eq!(job_cap(&host), 1);
        assert_eq!(jobs_line(&host), "jobs 0 / 1");
        assert!(!at_job_cap(&host));
        host.current_jobs = 1;
        assert_eq!(jobs_line(&host), "jobs 1 / 1");
        assert!(at_job_cap(&host));

        // Clamped advertised 256 still min'd with plane.
        host.max_concurrent = 2;
        host.advertised_max_concurrent = Some(300);
        host.current_jobs = 2;
        assert_eq!(job_cap(&host), 2);
        assert_eq!(jobs_line(&host), "jobs 2 / 2");
        assert!(at_job_cap(&host));

        // Running a job increments n; cap stays the min.
        host.max_concurrent = 8;
        host.advertised_max_concurrent = Some(4);
        host.current_jobs = 0;
        assert_eq!(jobs_line(&host), "jobs 0 / 4");
        host.current_jobs = 1;
        assert_eq!(jobs_line(&host), "jobs 1 / 4");
        assert_eq!(job_cap(&host), 4);
        assert!(!at_job_cap(&host));
        host.current_jobs = 4;
        assert_eq!(jobs_line(&host), "jobs 4 / 4");
        assert!(at_job_cap(&host));

        // Multi-host skip: at-min hosts drop out; under-cap peer stays.
        let mut under = sample_host();
        under.id = "under".into();
        under.hardware = None;
        under.max_concurrent = 8;
        under.advertised_max_concurrent = Some(2);
        under.current_jobs = 1;

        let mut at_adv = under.clone();
        at_adv.id = "at-adv".into();
        at_adv.current_jobs = 2;

        let mut at_plane = sample_host();
        at_plane.id = "at-plane".into();
        at_plane.hardware = None;
        at_plane.max_concurrent = 1;
        at_plane.advertised_max_concurrent = None;
        at_plane.current_jobs = 1;

        let mut plane_wins = sample_host();
        plane_wins.id = "plane-wins".into();
        plane_wins.hardware = None;
        plane_wins.max_concurrent = 2;
        plane_wins.advertised_max_concurrent = Some(8);
        plane_wins.current_jobs = 2;

        let mut zero_adv = sample_host();
        zero_adv.id = "zero-adv".into();
        zero_adv.hardware = None;
        zero_adv.max_concurrent = 8;
        zero_adv.advertised_max_concurrent = Some(0);
        zero_adv.current_jobs = 1;

        let pool = [under, at_adv, at_plane, plane_wins, zero_adv];
        assert!(!at_job_cap(&pool[0]));
        assert!(at_job_cap(&pool[1]), "advertised-cap host must be skipped");
        assert!(
            at_job_cap(&pool[2]),
            "plane-cap host (no advertised) must be skipped"
        );
        assert!(
            at_job_cap(&pool[3]),
            "min(plane=2, advertised=8) at 2 must be skipped"
        );
        assert!(
            at_job_cap(&pool[4]),
            "advertised 0 is not unlimited; 1 job must skip"
        );
        let ids: Vec<&str> = pool
            .iter()
            .filter(|h| !at_job_cap(h))
            .map(|h| h.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["under"],
            "only the under-cap host must stay assignable: {ids:?}"
        );
    }

    #[test]
    fn plane_card_jobs_line_uses_inflight_over_max_concurrent() {
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.advertised_max_concurrent = None;
        plane.hardware = None;
        assert!(is_control_plane(&plane));

        // Idle: n is in-flight local sessions, cap is the plane setting.
        plane.current_jobs = 0;
        plane.max_concurrent = 4;
        assert_eq!(job_cap(&plane), 4);
        assert_eq!(jobs_line(&plane), "jobs 0 / 4");

        // Must show in-flight n — do not lock a missing count as always 0/4.
        plane.current_jobs = 3;
        plane.max_concurrent = 8;
        assert_eq!(job_cap(&plane), 8);
        assert_eq!(jobs_line(&plane), "jobs 3 / 8");
        assert!(
            !jobs_line(&plane).contains("cpu")
                && !jobs_line(&plane).contains("mem")
                && !jobs_line(&plane).contains("gpu"),
            "{}",
            jobs_line(&plane)
        );

        plane.current_jobs = 8;
        assert_eq!(jobs_line(&plane), "jobs 8 / 8");

        plane.current_jobs = 9;
        assert_eq!(jobs_line(&plane), "jobs 9 / 8");

        plane.current_jobs = 1;
        plane.max_concurrent = 0;
        assert_eq!(jobs_line(&plane), "jobs 1 / 1");
        plane.max_concurrent = 512;
        assert_eq!(jobs_line(&plane), "jobs 1 / 256");

        plane.max_concurrent = 8;
        plane.current_jobs = 2;
        let patched = apply_patch(
            plane,
            &HostPatch {
                max_concurrent: Some(2),
                ..HostPatch::default()
            },
        );
        assert!(is_control_plane(&patched));
        assert_eq!(patched.current_jobs, 2);
        assert_eq!(job_cap(&patched), 2);
        assert_eq!(jobs_line(&patched), "jobs 2 / 2");
    }

    #[test]
    fn last_seen_label_plane_is_this_process() {
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.last_seen_at = Some("2026-08-20T12:00:00Z".into());
        assert_eq!(last_seen_label(&plane, 0.0), "this process");

        let mut host = sample_host();
        host.last_seen_at = None;
        assert_eq!(last_seen_label(&host, 0.0), "never");

        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:02Z").expect("now");
        host.last_seen_at = Some("2026-08-20T12:00:00Z".into());
        assert_eq!(
            last_seen_label(&host, now.timestamp_millis() as f64),
            "just now"
        );
    }

    #[test]
    fn relative_seen_covers_second_hour_day_buckets() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z").expect("now");
        let now_ms = now.timestamp_millis() as f64;
        assert_eq!(
            relative_seen(&Some("2026-08-20T11:59:50Z".into()), now_ms),
            "10s ago"
        );
        assert_eq!(
            relative_seen(&Some("2026-08-20T10:00:00Z".into()), now_ms),
            "2h ago"
        );
        assert_eq!(
            relative_seen(&Some("2026-08-17T12:00:00Z".into()), now_ms),
            "3d ago"
        );
        assert_eq!(
            relative_seen(&Some("not-a-timestamp".into()), now_ms),
            "not-a-timestamp"
        );
    }

    #[test]
    fn format_ts_replaces_t_or_emdash() {
        assert_eq!(format_ts(""), "—");
        assert_eq!(format_ts("2026-08-20T12:00:00Z"), "2026-08-20 12:00:00");
    }

    #[test]
    fn hint_is_loopback_detects_localhost_and_ipv6() {
        assert!(hint_is_loopback("FLEET_URL=http://127.0.0.1:8080"));
        assert!(hint_is_loopback("http://localhost:8080"));
        assert!(hint_is_loopback("http://[::1]:8080"));
        assert!(!hint_is_loopback("https://fleet.example.test"));
    }

    #[test]
    fn install_commands_include_url_and_pair_token() {
        let created = CreateHostResponse {
            host: sample_host(),
            pair_token: "flp_testdeadbeef".into(),
            pair_expires_at: String::new(),
            install_hint: String::new(),
        };
        let api = "http://localhost:8080";
        let unix = install_commands(&created, api);
        assert!(unix.contains("install-worker.sh"), "{unix}");
        assert!(unix.contains("FLEET_PAIR_TOKEN=flp_testdeadbeef"), "{unix}");
        assert!(unix.contains("FLEET_URL="), "{unix}");
        let win = windows_install_command(&created, api);
        assert!(win.contains("install-worker.ps1"), "{win}");
        assert!(win.contains("FLEET_PAIR_TOKEN="), "{win}");
        assert!(win.contains("flp_testdeadbeef"), "{win}");
    }

    #[test]
    fn install_commands_prefer_server_hint() {
        let created = CreateHostResponse {
            host: sample_host(),
            pair_token: "flp_hinttoken".into(),
            pair_expires_at: String::new(),
            install_hint: "curl -fsSL https://fleet.example.test/install-worker.sh | FLEET_URL=https://fleet.example.test FLEET_PAIR_TOKEN=flp_hinttoken sh".into(),
        };
        let unix = install_commands(&created, "http://127.0.0.1:8080");
        assert_eq!(unix, created.install_hint);
        assert_eq!(
            install_api_url(&created, "http://127.0.0.1:8080"),
            "https://fleet.example.test"
        );
        assert!(hint_is_loopback("http://127.0.0.1:8080"));
        assert!(!hint_is_loopback(&unix));
    }

    #[test]
    fn host_patch_drain_serializes_when_set() {
        let hold = HostPatch {
            paused: Some(true),
            ..HostPatch::default()
        };
        let hold_json = serde_json::to_value(&hold).expect("serialize hold");
        assert_eq!(hold_json["paused"], true);
        assert!(hold_json.get("drain").is_none());

        let release = HostPatch {
            paused: Some(false),
            drain: Some(false),
            ..HostPatch::default()
        };
        let release_json = serde_json::to_value(&release).expect("serialize release");
        assert_eq!(release_json["paused"], false);
        assert_eq!(release_json["drain"], false);
    }

    #[test]
    fn apply_patch_sets_and_clears_drain() {
        let host = sample_host();
        assert!(!host.drain);
        assert!(!host.paused);

        let held = apply_patch(
            host,
            &HostPatch {
                paused: Some(true),
                ..HostPatch::default()
            },
        );
        assert!(held.paused);
        assert!(!held.drain);

        let drained = apply_patch(
            held,
            &HostPatch {
                drain: Some(true),
                ..HostPatch::default()
            },
        );
        assert!(drained.drain);

        let released = apply_patch(
            drained,
            &HostPatch {
                paused: Some(false),
                drain: Some(false),
                ..HostPatch::default()
            },
        );
        assert!(!released.paused);
        assert!(!released.drain);
    }

    #[test]
    fn apply_patch_bumps_settings_pending_on_settings_change() {
        let host = sample_host();
        assert!(!settings_awaiting_ack(&host));
        let patched = apply_patch(
            host.clone(),
            &HostPatch {
                auto_update: Some(!host.auto_update),
                ..HostPatch::default()
            },
        );
        assert!(patched.settings_pending);
        assert_eq!(patched.settings_gen, host.settings_gen + 1);
        assert!(settings_awaiting_ack(&patched));
        assert_eq!(extra_status_pills(&patched), vec!["pending apply"]);

        let same = apply_patch(
            host.clone(),
            &HostPatch {
                auto_update: Some(host.auto_update),
                request_update: Some(true),
                ..HostPatch::default()
            },
        );
        assert!(!same.settings_pending);
        assert_eq!(same.settings_gen, host.settings_gen);
        assert!(!settings_awaiting_ack(&same));
    }

    /// catalog:unit-never-ack-stays-pending-apply — Old host that never acks
    /// stays pending apply. gen ≠ ack forever. Plane apply is immediate.
    #[test]
    fn old_host_that_never_acks_stays_pending_apply() {
        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.paired_at = Some("2026-01-01T00:00:00Z".into());
        host.settings_gen = 0;
        host.settings_ack_gen = 0;
        host.settings_pending = false;
        assert!(!settings_awaiting_ack(&host));
        assert!(extra_status_pills(&host).is_empty());
        assert!(!widgets_locked(&host, false));

        let applied = apply_patch(
            host,
            &HostPatch {
                auto_update: Some(false),
                ..HostPatch::default()
            },
        );
        assert!(applied.settings_pending);
        assert_eq!(applied.settings_gen, 1);
        assert_eq!(applied.settings_ack_gen, 0);
        assert_ne!(
            applied.settings_gen, applied.settings_ack_gen,
            "apply must leave gen ahead of ack"
        );
        assert!(settings_awaiting_ack(&applied));
        assert_eq!(extra_status_pills(&applied), vec!["pending apply"]);
        assert!(widgets_locked(&applied, false));

        // Old fleet-host: GET list / heartbeat never posts settings_ack_gen.
        // last_seen and inventory move; gen / ack / pending must not.
        let mut stale = applied;
        for i in 0..8 {
            stale.last_seen_at = Some(format!("2026-01-01T00:00:{i:02}Z"));
            stale.current_jobs = i % 3;
            stale.status = if i % 2 == 0 {
                "online".into()
            } else {
                "offline".into()
            };
            stale.hostname = Some("old-box".into());
            stale.platform = Some("linux-x86_64".into());
            stale.hardware = Some(HostHardware {
                cpu_cores: Some(8),
                ..HostHardware::default()
            });
            assert_eq!(
                stale.settings_gen, 1,
                "heartbeat must not bump settings_gen"
            );
            assert_eq!(
                stale.settings_ack_gen, 0,
                "old host must never advance settings_ack_gen"
            );
            assert!(
                stale.settings_pending,
                "gen != ack forever → settings_pending true"
            );
            assert!(settings_awaiting_ack(&stale));
            assert_eq!(
                extra_status_pills(&stale),
                vec!["pending apply"],
                "old host never acks — pending apply must stay (i={i})"
            );
            assert!(widgets_locked(&stale, false));
        }

        // No-op CP apply / Update now must not ack or rewind gen.
        stale = apply_patch(
            stale,
            &HostPatch {
                auto_update: Some(false),
                request_update: Some(true),
                ..HostPatch::default()
            },
        );
        assert_eq!(
            stale.settings_gen, 1,
            "never-ack must not bump or rewind settings_gen"
        );
        assert_eq!(stale.settings_ack_gen, 0);
        assert!(stale.settings_pending);
        assert_eq!(extra_status_pills(&stale), vec!["pending apply"]);

        // Another real settings apply: gen moves further ahead; ack still 0.
        let again = apply_patch(
            stale,
            &HostPatch {
                max_concurrent: Some(2),
                ..HostPatch::default()
            },
        );
        assert_eq!(again.settings_gen, 2);
        assert_eq!(again.settings_ack_gen, 0);
        assert!(
            again.settings_pending,
            "gen != ack forever → settings_pending true"
        );
        assert_eq!(extra_status_pills(&again), vec!["pending apply"]);

        // The flag can lag; gen != ack is what keeps pending apply forever.
        let mut lagged = again.clone();
        lagged.settings_pending = false;
        assert_ne!(lagged.settings_gen, lagged.settings_ack_gen);
        assert!(settings_awaiting_ack(&lagged));
        assert_eq!(extra_status_pills(&lagged), vec!["pending apply"]);
        assert!(widgets_locked(&lagged, false));

        // Control: a host that does ack (modern fleet-host) clears the pill.
        let mut acked = again.clone();
        acked.settings_ack_gen = acked.settings_gen;
        acked.settings_pending = false;
        assert!(!settings_awaiting_ack(&acked));
        assert!(
            extra_status_pills(&acked).is_empty(),
            "matching ack must clear pending apply: {:?}",
            extra_status_pills(&acked)
        );

        // Plane apply is this process — no pending apply, widgets stay live.
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.settings_gen = 0;
        plane.settings_ack_gen = 0;
        plane.settings_pending = false;
        let plane_applied = apply_patch(
            plane,
            &HostPatch {
                max_concurrent: Some(8),
                paused: Some(true),
                ..HostPatch::default()
            },
        );
        assert!(is_control_plane(&plane_applied));
        assert_eq!(plane_applied.max_concurrent, 8);
        assert!(
            !plane_applied.settings_pending,
            "plane apply is immediate — no settings_pending"
        );
        assert_eq!(
            plane_applied.settings_gen, 0,
            "plane has no host ack cycle — apply must not bump settings_gen"
        );
        assert_eq!(plane_applied.settings_ack_gen, 0);
        assert_eq!(
            plane_applied.settings_gen, plane_applied.settings_ack_gen,
            "plane apply acks in the same step"
        );
        assert!(!settings_awaiting_ack(&plane_applied));
        assert!(
            extra_status_pills(&plane_applied).is_empty(),
            "plane apply is immediate (no pending apply): {:?}",
            extra_status_pills(&plane_applied)
        );
        assert!(
            !widgets_locked(&plane_applied, false),
            "plane apply is immediate — widgets stay live"
        );
    }

    /// catalog:unit-request-update — Auto-update off + Update now (`request_update`).
    #[test]
    fn apply_patch_request_update_marks_host() {
        let mut host = sample_host();
        host.auto_update = false;
        host.kind = "host".into();
        host.software_status = Some("behind".into());
        assert!(host.update_requested_at.is_none());
        assert_eq!(update_now_label(&host, false), Some("Update now"));
        assert_eq!(update_now_label(&host, true), None);

        let ignored = apply_patch(
            host,
            &HostPatch {
                request_update: Some(false),
                auto_update: Some(false),
                ..HostPatch::default()
            },
        );
        assert!(!update_requested(&ignored));
        assert!(!ignored.auto_update);
        assert_eq!(ignored.settings_gen, 0);
        assert_eq!(update_now_label(&ignored, false), Some("Update now"));

        let marked = apply_patch(
            ignored,
            &HostPatch {
                request_update: Some(true),
                ..HostPatch::default()
            },
        );
        assert_eq!(marked.update_requested_at.as_deref(), Some("requested"));
        assert!(update_requested(&marked));
        assert!(!marked.auto_update, "Update now must not flip auto-update");
        assert_eq!(
            marked.settings_gen, 0,
            "Update now is a one-shot, not a settings bump"
        );
        assert_eq!(update_now_label(&marked, false), Some("Update requested"));

        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.auto_update = false;
        plane.software_status = Some("behind".into());
        assert_eq!(update_now_label(&plane, false), None);

        let json = serde_json::to_value(&HostPatch {
            request_update: Some(true),
            ..HostPatch::default()
        })
        .expect("serialize");
        assert_eq!(json["request_update"], true);
        assert!(json.get("auto_update").is_none());
    }

    #[test]
    fn clipboard_helper_rejects_empty_payload() {
        assert!(!clipboard_has_payload(""));
        assert!(clipboard_has_payload("flp_token"));
        assert!(clipboard_has_payload(
            "curl -fsSL http://x/install-worker.sh"
        ));
        assert!(clipboard_has_payload(
            "irm \"$env:FLEET_URL/install-worker.ps1\" | iex"
        ));
    }

    /// catalog:e2e-copy-buttons — just test excludes fleet-ui. Lock the live
    /// PairPanel so a silent no-op or clipboard-API-only Copy still fails here.
    #[test]
    fn pair_panel_copy_buttons_use_http_lan_helper() {
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let idx = prod.find("fn PairPanel").expect("PairPanel");
        let after = &prod[idx..];
        let end = after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(after.len());
        let pair = &after[..end];
        assert!(
            prod.contains("use crate::clipboard::copy_text"),
            "pair panel must use the HTTP LAN clipboard helper"
        );
        assert!(
            !prod.contains("fn copy_text"),
            "page.rs must not shadow copy_text"
        );
        assert!(
            pair.contains("if copy_text(&unix_copy)")
                && pair.contains("if copy_text(&windows_copy)")
                && pair.contains("if copy_text(&token)"),
            "Unix, Windows, and token Copy must call copy_text"
        );
        assert!(
            !pair.contains("let _ = copy_text"),
            "must not ignore copy_text — HTTP LAN no-op would still toast success"
        );
        assert!(
            !pair.contains("navigator.clipboard") && !pair.contains("navigator().clipboard"),
            "must not read navigator.clipboard in PairPanel (Firefox HTTP LAN panics)"
        );
        assert!(
            pair.contains("\"Command copied.\"") && pair.contains("\"Token copied.\""),
            "success toast is how the e2e knows the fallback wrote"
        );
        assert!(
            pair.contains("\"Copy failed. Select the command and copy it yourself.\"")
                && pair.contains("\"Copy failed. Select the token and copy it yourself.\""),
            "failed copy must not look like success"
        );
    }

    #[test]
    fn identity_line_is_hostname_dot_platform() {
        let mut host = sample_host();
        assert!(identity_line(&host).is_none());
        host.hostname = Some("  ".into());
        host.platform = Some(String::new());
        assert!(identity_line(&host).is_none());

        host.hostname = Some("box.local".into());
        assert_eq!(identity_line(&host).as_deref(), Some("box.local"));

        host.hostname = None;
        host.platform = Some("linux-x86_64".into());
        assert_eq!(identity_line(&host).as_deref(), Some("linux-x86_64"));

        host.hostname = Some("box.local".into());
        assert_eq!(
            identity_line(&host).as_deref(),
            Some("box.local · linux-x86_64")
        );
    }

    #[test]
    fn settings_draft_dirty_ignores_plane_auto_update() {
        let mut host = sample_host();
        host.allow_host = true;
        host.allow_container = true;
        host.auto_update = true;
        host.max_concurrent = 4;
        assert!(!settings_draft_dirty(&host, true, true, 4));
        assert!(settings_draft_dirty(&host, false, true, 4));
        assert!(settings_draft_dirty(&host, true, false, 4));
        assert!(settings_draft_dirty(&host, true, true, 8));

        host.kind = "control_plane".into();
        assert!(
            !settings_draft_dirty(&host, true, false, 4),
            "plane has no auto-update widget"
        );
        assert!(settings_draft_dirty(&host, false, false, 4));
    }

    /// catalog:list-settings-draft-dirty-until-apply
    /// catalog:unit-settings-draft-dirty — Edits stay local until Apply.
    /// Toggle auto-update without Apply must not look applied. Dirty vs
    /// pending are distinct.
    #[test]
    fn settings_draft_dirty_stays_local_until_apply() {
        // just test excludes fleet-ui; lock the live RSX so a checkbox that
        // PATCHes immediately, or Apply gated on pending, still fails here.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("settings_draft_dirty"),
            "production source must import settings_draft_dirty"
        );
        assert!(
            !prod.contains("fn settings_draft_dirty"),
            "page.rs must call the crate helper, not a local shadow"
        );

        let idx = prod
            .find("fn HostDetailPanel")
            .expect("HostDetailPanel in production source");
        let after = &prod[idx..];
        let end = after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(after.len());
        let detail = &after[..end];

        assert!(
            detail.contains("use_signal(|| host.auto_update)"),
            "auto-update draft must start from the last applied host"
        );
        let dirty_bind = detail
            .lines()
            .find(|l| l.contains("settings_draft_dirty("))
            .expect("dirty binding from settings_draft_dirty");
        assert!(
            dirty_bind.contains("d_auto()"),
            "dirty must compare the auto-update draft, not only the applied host: {dirty_bind}"
        );
        assert!(
            detail.contains("let pending = widgets_locked"),
            "pending is widgets_locked after Apply, not the dirty flag"
        );

        let auto_at = detail
            .find("\"Auto-update\"")
            .expect("Auto-update label in HostDetailPanel");
        let auto_gate = detail[..auto_at]
            .rfind("if shows_auto_update(&host)")
            .expect("if shows_auto_update(&host) before Auto-update");
        let auto_block = &detail[auto_gate..auto_at];
        assert!(
            auto_block.contains("checked: d_auto()"),
            "checkbox must bind the draft, not host.auto_update"
        );
        assert!(
            auto_block.contains("d_auto.set("),
            "checkbox must write the local draft only"
        );
        assert!(
            !auto_block.contains("patch_host")
                && !auto_block.contains("apply_patch")
                && !auto_block.contains("HostPatch")
                && !auto_block.contains("spawn("),
            "toggling Auto-update must not PATCH / Apply"
        );

        let apply_at = detail
            .find("\"Apply changes\"")
            .expect("Apply changes button in HostDetailPanel");
        let apply_gate_at = detail[..apply_at]
            .rfind("if dirty")
            .expect("if dirty before Apply changes");
        let apply_gate = detail[apply_gate_at..].lines().next().unwrap_or("");
        assert!(
            apply_gate.contains("dirty") && !apply_gate.contains("pending"),
            "Apply appears for local dirty, not for pending apply: {apply_gate:?}"
        );
        let apply_block = &detail[apply_gate_at..apply_at];
        assert!(
            apply_block.contains("auto_update: Some(d_auto())"),
            "Apply is what sends the auto-update draft"
        );
        assert!(
            apply_block.contains("patch_host"),
            "Apply is the PATCH; the checkbox is not"
        );

        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.allow_host = true;
        host.allow_container = true;
        host.auto_update = true;
        host.max_concurrent = 4;
        host.settings_gen = 3;
        host.settings_ack_gen = 3;
        host.settings_pending = false;
        let snapshot = host.clone();
        let accepting = accepting_jobs(&host);
        assert_eq!(accepting, true);
        assert!(!settings_awaiting_ack(&host));
        assert!(!widgets_locked(&host, false));
        assert!(extra_status_pills(&host).is_empty());
        assert!(!settings_draft_dirty(
            &host,
            accepting,
            host.auto_update,
            host.max_concurrent
        ));

        // Draft toggle only: host row, gen, pending pill, and widget lock stay put.
        let draft_auto = !host.auto_update;
        assert!(
            settings_draft_dirty(&host, accepting, draft_auto, host.max_concurrent),
            "flipped auto-update draft must be dirty"
        );
        assert_eq!(
            host, snapshot,
            "dirty is a local compare — host is unchanged"
        );
        assert_eq!(
            host.auto_update, true,
            "toggle without Apply is not applied"
        );
        assert_eq!(host.settings_gen, snapshot.settings_gen);
        assert_eq!(host.settings_ack_gen, snapshot.settings_ack_gen);
        assert!(!host.settings_pending);
        assert!(
            !settings_awaiting_ack(&host),
            "dirty must not be pending apply"
        );
        assert!(
            extra_status_pills(&host).is_empty(),
            "toggle without Apply must not show pending apply: {:?}",
            extra_status_pills(&host)
        );
        assert!(
            !widgets_locked(&host, false),
            "dirty must not lock widgets — that is pending"
        );
        // Revert the draft: clean again, still never Applied.
        assert!(!settings_draft_dirty(
            &host,
            accepting,
            host.auto_update,
            host.max_concurrent
        ));
        assert_eq!(host, snapshot);

        // Apply is what writes auto_update and starts the ack cycle.
        let applied = apply_patch(
            host.clone(),
            &HostPatch {
                auto_update: Some(draft_auto),
                ..HostPatch::default()
            },
        );
        assert_eq!(applied.auto_update, draft_auto);
        assert_ne!(applied.auto_update, host.auto_update);
        assert!(applied.settings_pending);
        assert_eq!(applied.settings_gen, host.settings_gen + 1);
        assert_eq!(applied.settings_ack_gen, host.settings_ack_gen);
        assert!(settings_awaiting_ack(&applied));
        assert_eq!(extra_status_pills(&applied), vec!["pending apply"]);
        assert!(widgets_locked(&applied, false));
        assert!(
            !settings_draft_dirty(
                &applied,
                accepting_jobs(&applied),
                applied.auto_update,
                applied.max_concurrent
            ),
            "after Apply the draft matches the host — dirty is gone; pending remains"
        );
        assert_ne!(
            settings_draft_dirty(
                &applied,
                accepting_jobs(&applied),
                applied.auto_update,
                applied.max_concurrent
            ),
            settings_awaiting_ack(&applied),
            "dirty vs pending must disagree after Apply, before ack"
        );

        // Ack clears pending; dirty stays false.
        let mut acked = applied.clone();
        acked.settings_ack_gen = acked.settings_gen;
        acked.settings_pending = false;
        assert!(!settings_awaiting_ack(&acked));
        assert!(extra_status_pills(&acked).is_empty());
        assert!(!widgets_locked(&acked, false));
        assert!(!settings_draft_dirty(
            &acked,
            accepting_jobs(&acked),
            acked.auto_update,
            acked.max_concurrent
        ));
    }

    /// catalog:list-settings-widgets-lock-while-pending
    /// catalog:unit-widgets-lock — Widgets lock while pending. The
    /// settings_pending helper (`settings_awaiting_ack`) is gen ahead of
    /// ack, not only the API flag. Isolation + auto-update + cap stay
    /// locked until ack. Plane apply is immediate (no pending).
    #[test]
    fn settings_pending_gen_ahead_of_ack_locks_widgets() {
        // just test excludes fleet-ui; lock the live RSX so a helper that
        // only reads the settings_pending flag, or widgets that ignore
        // pending, cannot go green.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("widgets_locked"),
            "production source must import widgets_locked"
        );
        assert!(
            !prod.contains("fn widgets_locked") && !prod.contains("fn settings_awaiting_ack"),
            "page.rs must call the crate helpers, not a local shadow"
        );
        assert!(
            !prod.contains("host.settings_pending")
                && !prod.contains("host.settings_gen")
                && !prod.contains("host.settings_ack_gen"),
            "widgets must lock via widgets_locked / settings_awaiting_ack, not the lagging flag"
        );

        let idx = prod
            .find("fn HostDetailPanel")
            .expect("HostDetailPanel in production source");
        let after = &prod[idx..];
        let end = after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(after.len());
        let detail = &after[..end];
        assert!(
            detail.contains("let pending = widgets_locked"),
            "pending is widgets_locked after Apply, not the dirty flag"
        );

        let auto_at = detail
            .find("\"Auto-update\"")
            .expect("Auto-update label in HostDetailPanel");
        let auto_gate = detail[..auto_at]
            .rfind("if shows_auto_update(&host)")
            .expect("if shows_auto_update(&host) before Auto-update");
        let auto_block = &detail[auto_gate..auto_at];
        assert!(
            auto_block.contains("disabled: busy() || pending"),
            "Auto-update must lock while settings_pending / gen != ack: {auto_block}"
        );

        let cap_at = detail
            .find("\"Max jobs at once\"")
            .expect("Max jobs at once in HostDetailPanel");
        let cap_end = (cap_at + 400).min(detail.len());
        let cap_block = &detail[cap_at..cap_end];
        assert!(
            cap_block.contains("disabled: busy() || pending"),
            "Max jobs at once must lock while settings_pending / gen != ack: {cap_block}"
        );

        let sb_call_at = detail
            .find("SandboxBlock")
            .expect("SandboxBlock in HostDetailPanel");
        let sb_call_end = (sb_call_at + 240).min(detail.len());
        let sb_call = &detail[sb_call_at..sb_call_end];
        assert!(
            sb_call.contains("pending,"),
            "isolation SandboxBlock must receive pending: {sb_call}"
        );

        let sb_idx = prod
            .find("fn SandboxBlock")
            .expect("SandboxBlock in production source");
        let sb_after = &prod[sb_idx..];
        let sb_end = sb_after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(sb_after.len());
        let sandbox = &sb_after[..sb_end];
        for needle in [
            "\"Allow process jobs\"",
            "\"Allow container jobs\"",
            "\"Work root\"",
            "\"Max job timeout (s)\"",
        ] {
            let at = sandbox
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} in SandboxBlock"));
            let start = at.saturating_sub(320);
            let end = (at + needle.len() + 420).min(sandbox.len());
            let window = &sandbox[start..end];
            assert!(
                window.contains("disabled: pending"),
                "{needle} must lock while pending: {window}"
            );
        }

        let mut host = sample_host();
        host.kind = "host".into();
        host.status = "online".into();
        host.settings_gen = 3;
        host.settings_ack_gen = 3;
        host.settings_pending = false;
        assert!(!settings_awaiting_ack(&host));
        assert!(!widgets_locked(&host, false));

        // The flag can lag; gen ahead of ack is what locks widgets.
        host.settings_gen = 4;
        host.settings_ack_gen = 3;
        host.settings_pending = false;
        assert_ne!(host.settings_gen, host.settings_ack_gen);
        assert!(
            settings_awaiting_ack(&host),
            "settings_pending helper must treat gen ahead of ack as pending"
        );
        assert!(
            widgets_locked(&host, false),
            "settings_gen != ack must lock isolation + auto-update + cap"
        );

        host.settings_gen = 3;
        host.settings_ack_gen = 3;
        host.settings_pending = true;
        assert!(settings_awaiting_ack(&host));
        assert!(widgets_locked(&host, false));

        let mut clean = sample_host();
        clean.kind = "host".into();
        clean.status = "online".into();
        clean.settings_gen = 1;
        clean.settings_ack_gen = 1;
        clean.settings_pending = false;
        clean.auto_update = true;
        let applied = apply_patch(
            clean.clone(),
            &HostPatch {
                auto_update: Some(false),
                ..HostPatch::default()
            },
        );
        assert!(applied.settings_gen > applied.settings_ack_gen);
        assert!(applied.settings_pending);
        assert!(settings_awaiting_ack(&applied));
        assert!(widgets_locked(&applied, false));

        let mut lagged = applied.clone();
        lagged.settings_pending = false;
        assert_ne!(lagged.settings_gen, lagged.settings_ack_gen);
        assert!(settings_awaiting_ack(&lagged));
        assert!(widgets_locked(&lagged, false));

        let mut acked = applied.clone();
        acked.settings_ack_gen = acked.settings_gen;
        acked.settings_pending = false;
        assert!(!settings_awaiting_ack(&acked));
        assert!(!widgets_locked(&acked, false));

        // Plane apply is this process — no pending lock even if gen != ack.
        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.status = "online".into();
        plane.settings_gen = 4;
        plane.settings_ack_gen = 1;
        plane.settings_pending = true;
        assert!(is_control_plane(&plane));
        assert!(
            !widgets_locked(&plane, false),
            "plane apply is immediate — widgets stay live"
        );
        assert!(
            extra_status_pills(&plane).is_empty(),
            "plane has no pending apply: {:?}",
            extra_status_pills(&plane)
        );

        let mut plane_clean = sample_host();
        plane_clean.kind = "control_plane".into();
        plane_clean.status = "online".into();
        plane_clean.settings_gen = 0;
        plane_clean.settings_ack_gen = 0;
        plane_clean.settings_pending = false;
        let plane_applied = apply_patch(
            plane_clean,
            &HostPatch {
                max_concurrent: Some(8),
                auto_update: Some(false),
                ..HostPatch::default()
            },
        );
        assert!(
            !plane_applied.settings_pending,
            "plane apply is immediate — no settings_pending"
        );
        assert_eq!(
            plane_applied.settings_gen, plane_applied.settings_ack_gen,
            "plane has no host ack cycle"
        );
        assert!(!settings_awaiting_ack(&plane_applied));
        assert!(
            !widgets_locked(&plane_applied, false),
            "plane apply is immediate — widgets stay live"
        );
    }

    #[test]
    fn widgets_lock_while_pending_not_on_plane() {
        let mut host = sample_host();
        assert!(!widgets_locked(&host, false));
        assert!(widgets_locked(&host, true));

        host.settings_pending = true;
        assert!(widgets_locked(&host, false));

        host.settings_pending = false;
        host.settings_gen = 2;
        host.settings_ack_gen = 1;
        assert!(widgets_locked(&host, false));

        host.kind = "control_plane".into();
        assert!(
            !widgets_locked(&host, false),
            "plane apply is immediate — widgets stay live"
        );
        assert!(widgets_locked(&host, true));
    }

    /// catalog:list-runtime-line-sandbox-backend-only
    /// catalog:unit-runtime-line — One runtime: SANDBOX_BACKEND on the plane;
    /// process on a paired host. Helper must not offer Native/Sandboxed
    /// checkboxes as a second runtime picker.
    #[test]
    fn runtime_line_is_sandbox_backend_not_isolation_modes() {
        // just test excludes fleet-ui; lock the live RSX so swapping
        // runtime_line for Native/Sandboxed checkboxes cannot go green.
        let src = include_str!("../../fleet-ui/src/pages/runners/self_hosted/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page.rs production source");
        let import = prod
            .split("use fleet_runners_view::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("fleet_runners_view import in production source");
        assert!(
            import.contains("runtime_line"),
            "production source must import runtime_line"
        );
        assert!(
            !prod.contains("fn runtime_line"),
            "page.rs must call the crate helper, not a local shadow"
        );
        assert_eq!(
            prod.matches("runtime_line(&host, &sandbox_backend)")
                .count(),
            2,
            "list row + detail must both call runtime_line"
        );
        assert_eq!(
            prod.matches("\"{runtime}\"").count(),
            2,
            "list row + detail must render the runtime_line string"
        );
        assert!(
            !prod.contains("RunnerModeChecks")
                && !prod.contains("ISOLATION_LABEL")
                && !prod.contains("SANDBOXED_HINT")
                && !prod.contains("\"Sandboxed\""),
            "must not offer Native/Sandboxed isolation checkboxes"
        );
        assert!(
            !prod.contains("\"Isolation\""),
            "Isolation section was the Native/Sandboxed picker"
        );
        // Accessible-name Native checkbox (label text). SandboxBlock's
        // <p> "Native" heading is not a checkbox name.
        let native_checkbox = prod.split("r#type: \"checkbox\"").skip(1).any(|after| {
            after
                .lines()
                .take(12)
                .any(|l| l.contains("\"Native\"") && !l.contains("font-medium"))
        });
        assert!(
            !native_checkbox,
            "must not offer a Native isolation checkbox"
        );
        assert!(
            !prod.contains("Control plane does not run sandboxes")
                && !prod.contains("Runtime: fleet-host")
                && !prod.contains("Runtime: process on this machine"),
            "page.rs must render runtime_line, not a hardcoded runtime string"
        );

        let lib = include_str!("lib.rs");
        let lib_prod = lib.split("#[cfg(test)]").next().expect("lib production");
        assert!(
            !lib_prod.contains("ISOLATION_LABEL")
                && !lib_prod.contains("SANDBOXED_HINT")
                && !lib_prod.contains("fn plane_native_locked"),
            "helper must not offer a second runtime picker"
        );
        let runtime_fn = {
            let start = lib_prod
                .find("pub fn runtime_line")
                .expect("pub fn runtime_line");
            let rest = &lib_prod[start..];
            let end = rest.find("\npub fn ").unwrap_or(rest.len());
            &rest[..end]
        };
        assert!(
            runtime_fn.contains("sandbox_backend")
                && !runtime_fn.contains("let _ = sandbox_backend"),
            "plane runtime_line must display workspace SANDBOX_BACKEND, not discard it"
        );
        assert!(
            !runtime_fn.contains("does not run sandboxes")
                && !runtime_fn.contains("Runtime: fleet-host"),
            "must not hide the env backend behind weakened copy"
        );
        assert!(
            !runtime_fn.contains("isolation")
                && !runtime_fn.contains("allow_host")
                && !runtime_fn.contains("allow_container"),
            "runtime_line is not an isolation / allow_* picker"
        );

        // Missing kind is a paired host (process), not the plane.
        let bare = sample_host();
        assert!(
            bare.kind.is_empty(),
            "sample host kind defaults empty — lock the non-plane path"
        );
        assert_eq!(
            runtime_line(&bare, "k8s"),
            "Runtime: process on this machine"
        );

        let mut host = sample_host();
        host.kind = "host".into();
        host.is_control_plane_box = false;
        // Paired host is process. sandbox_backend is the plane env — it
        // must not change the host line or become a second picker.
        for backend in ["docker", "process", "stub", "k8s", "local", "", "  vm  "] {
            let line = runtime_line(&host, backend);
            assert_eq!(
                line, "Runtime: process on this machine",
                "paired host is process, not {backend:?}: {line}"
            );
            assert!(
                !line.to_ascii_lowercase().contains("native")
                    && !line.to_ascii_lowercase().contains("sandboxed")
                    && !line.contains("SANDBOX_BACKEND"),
                "paired host is not a runtime picker: {line}"
            );
        }
        // Isolation field / allow_* are not a second runtime picker.
        host.isolation = Some("sandboxed".into());
        host.allow_host = false;
        host.allow_container = true;
        assert_eq!(
            runtime_line(&host, "docker"),
            "Runtime: process on this machine",
            "paired host stays process regardless of isolation/allow_*"
        );
        host.isolation = Some("native".into());
        host.allow_host = true;
        host.allow_container = false;
        assert_eq!(
            runtime_line(&host, "k8s"),
            "Runtime: process on this machine"
        );
        host.is_control_plane_box = true;
        assert_eq!(
            runtime_line(&host, "docker"),
            "Runtime: process on this machine",
            "kind=host stays process even on the plane machine"
        );

        let mut plane = sample_host();
        plane.kind = "control_plane".into();
        plane.is_control_plane_box = false;
        assert_eq!(
            runtime_line(&plane, "process"),
            "Runtime: process (SANDBOX_BACKEND)"
        );
        assert_eq!(
            runtime_line(&plane, "docker"),
            "Runtime: docker (SANDBOX_BACKEND)"
        );
        assert_eq!(
            runtime_line(&plane, "stub"),
            "Runtime: stub (SANDBOX_BACKEND)"
        );
        assert_eq!(
            runtime_line(&plane, "local"),
            "Runtime: local (SANDBOX_BACKEND)"
        );
        assert_eq!(
            runtime_line(&plane, "k8s"),
            "Runtime: k8s (SANDBOX_BACKEND)"
        );
        assert_eq!(
            runtime_line(&plane, ""),
            "Runtime: stub (SANDBOX_BACKEND)",
            "empty SANDBOX_BACKEND displays as stub"
        );
        assert_eq!(
            runtime_line(&plane, "   "),
            "Runtime: stub (SANDBOX_BACKEND)",
            "whitespace-only SANDBOX_BACKEND displays as stub"
        );
        assert_eq!(
            runtime_line(&plane, "  process  "),
            "Runtime: process (SANDBOX_BACKEND)",
            "trim workspace SANDBOX_BACKEND"
        );
        plane.isolation = Some("sandboxed".into());
        plane.allow_host = false;
        assert_eq!(
            runtime_line(&plane, "docker"),
            "Runtime: docker (SANDBOX_BACKEND)",
            "plane isolation/allow_* must not replace SANDBOX_BACKEND"
        );
        plane.is_control_plane_box = true;
        assert_eq!(
            runtime_line(&plane, "process"),
            "Runtime: process (SANDBOX_BACKEND)",
            "kind=control_plane is the gate"
        );

        // Weakened copy that hid SANDBOX_BACKEND must not pass.
        for backend in ["process", "docker", "stub", ""] {
            let line = runtime_line(&plane, backend);
            assert!(
                line.contains("SANDBOX_BACKEND"),
                "plane runtime_line must show SANDBOX_BACKEND, got {line}"
            );
            assert!(
                !line.contains("does not run sandboxes"),
                "plane must show the env backend, not hide it: {line}"
            );
            assert!(
                !line.to_ascii_lowercase().contains("native")
                    && !line.to_ascii_lowercase().contains("sandboxed"),
                "runtime_line is not an isolation picker: {line}"
            );
        }

        assert_eq!(BEHAVIOR_LABEL, "Behavior");
    }

    #[test]
    fn draft_holding_and_clamp_max_jobs() {
        assert!(draft_holding_new_jobs(false));
        assert!(!draft_holding_new_jobs(true));
        assert_eq!(clamp_max_jobs(0), 1);
        assert_eq!(clamp_max_jobs(1), 1);
        assert_eq!(clamp_max_jobs(4), 4);
        assert_eq!(clamp_max_jobs(256), 256);
        assert_eq!(clamp_max_jobs(512), 256);
    }

    #[test]
    fn specs_line_formats_vram_and_multiple_gpus() {
        let mut host = sample_host();
        host.hardware = Some(HostHardware {
            cpu_model: Some("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz".into()),
            cpu_cores: Some(1),
            mem_total_bytes: Some(16 * 1_073_741_824),
            mem_speed_mts: Some(2666),
            mem_type: None,
            gpus: Some(vec![
                HostGpu {
                    name: Some("Intel UHD Graphics".into()),
                    vram_bytes: None,
                    used_pct: Some(4),
                },
                HostGpu {
                    name: Some("NVIDIA RTX 3080".into()),
                    vram_bytes: Some(10 * 1_073_741_824),
                    used_pct: Some(40),
                },
                HostGpu {
                    name: Some("  ".into()),
                    vram_bytes: Some(8 * 1_073_741_824),
                    used_pct: None,
                },
            ]),
            ..HostHardware::default()
        });
        let line = specs_line(&host).expect("specs");
        assert!(line.contains("i7-10610U"), "{line}");
        assert!(line.contains("1 core"), "{line}");
        assert!(!line.contains("1 cores"), "{line}");
        assert!(line.contains("16 GB 2666 MT/s"), "{line}");
        assert!(line.contains("Intel UHD Graphics"), "{line}");
        assert!(line.contains("NVIDIA RTX 3080 10 GB"), "{line}");
        assert!(!line.contains("  "), "blank gpu name must drop: {line}");
    }

    #[test]
    fn hint_is_loopback_does_not_flag_lan_or_public() {
        assert!(hint_is_loopback("http://127.0.0.1:8080"));
        assert!(hint_is_loopback("http://localhost:8080"));
        assert!(hint_is_loopback("http://[::1]:8080"));
        assert!(!hint_is_loopback("http://192.168.1.10:8080"));
        assert!(!hint_is_loopback("http://10.0.0.4:8080"));
        assert!(!hint_is_loopback("https://fleet.example.test"));
        assert!(!hint_is_loopback("http://172.16.0.8:8080"));
    }

    #[test]
    fn software_title_covers_bins_and_unknown() {
        let mut host = sample_host();
        assert_eq!(
            software_title(&host),
            "This box has not reported fleet-host and fleet-driver versions we can compare."
        );

        host.current_versions = PackageVersions {
            host: "0.0.1".into(),
            driver: "0.0.1".into(),
        };
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":{"sha256":"h","version":"0.0.1"},"fleet-driver":{"sha256":"d","version":"0.0.1"}}}"#,
        )
        .expect("software");
        host.platform = Some("linux-x86_64".into());
        assert!(
            software_title(&host).contains("fleet-host and fleet-driver"),
            "{}",
            software_title(&host)
        );
        assert!(
            software_title(&host).contains("linux-x86_64"),
            "{}",
            software_title(&host)
        );

        host.platform = None;
        host.desired = Some(HostDesiredSoftware {
            platform: Some("windows-x86_64".into()),
            ..HostDesiredSoftware::default()
        });
        assert!(
            software_title(&host).contains("windows-x86_64"),
            "{}",
            software_title(&host)
        );
    }

    #[test]
    fn needs_software_update_and_empty_package_report() {
        let mut host = sample_host();
        assert!(!needs_software_update(&host));
        host.outdated = true;
        assert!(needs_software_update(&host));
        host.outdated = false;
        host.software_status = Some("behind".into());
        assert!(needs_software_update(&host));
        host.software_status = Some("current".into());
        assert!(
            !needs_software_update(&host),
            "current means both binaries match; bake id is not a version"
        );

        assert!(served_platforms(&PackageInventory::default()).is_empty());
    }

    #[test]
    fn apply_patch_isolation_and_cap() {
        let host = sample_host();
        assert!(host.allow_host);
        assert!(host.allow_container);
        let patched = apply_patch(
            host.clone(),
            &HostPatch {
                allow_host: Some(false),
                allow_container: Some(false),
                max_concurrent: Some(1),
                ..HostPatch::default()
            },
        );
        assert!(!patched.allow_host);
        assert!(!patched.allow_container);
        assert_eq!(patched.max_concurrent, 1);
        assert!(patched.settings_pending);
        assert_eq!(patched.settings_gen, host.settings_gen + 1);
        assert!(holding_new_jobs(&patched));
    }

    #[test]
    fn windows_install_does_not_hardcode_arch() {
        let created = CreateHostResponse {
            host: sample_host(),
            pair_token: "flp_arch".into(),
            pair_expires_at: String::new(),
            install_hint: String::new(),
        };
        let win = windows_install_command(&created, "https://fleet.example.test");
        assert!(win.contains("install-worker.ps1"), "{win}");
        assert!(win.contains("FLEET_PAIR_TOKEN='flp_arch'"), "{win}");
        assert!(
            !win.to_ascii_lowercase().contains("x64")
                && !win.to_ascii_lowercase().contains("amd64")
                && !win.to_ascii_lowercase().contains("arm64"),
            "arch comes from the machine, not the command: {win}"
        );
    }
}
