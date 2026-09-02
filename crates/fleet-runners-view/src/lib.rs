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
pub fn software_build_id(host: &HostPublic) -> Option<&str> {
    host.software
        .as_ref()
        .and_then(|s| s.git_sha.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
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

/// One served binary vs what this box reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareBinRow {
    pub name: &'static str,
    /// Package semver the box reported (file hash only if no version yet).
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
            let version = got_ver
                .or(want_ver)
                .or_else(|| got.as_deref().map(short_sha).map(str::to_string))
                .unwrap_or_else(|| "—".into());
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
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 8 {
        hex[..8].to_string()
    } else {
        raw.chars().take(8).collect()
    }
}

pub fn job_cap(host: &HostPublic) -> i32 {
    let cp = host.max_concurrent.clamp(1, 256);
    match host.advertised_max_concurrent {
        Some(n) => cp.min(n.clamp(1, 256)),
        None => cp,
    }
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

/// Control plane first, then status rank, then name.
pub fn cmp_hosts_for_list(a: &HostPublic, b: &HostPublic) -> std::cmp::Ordering {
    is_control_plane(b)
        .cmp(&is_control_plane(a))
        .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
        .then_with(|| a.name.cmp(&b.name))
}

pub fn sort_hosts_for_list(hosts: &mut [HostPublic]) {
    hosts.sort_by(cmp_hosts_for_list);
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

/// How this runner executes. Isolation is process < container < vm on the host.
pub fn runtime_line(host: &HostPublic) -> String {
    if is_control_plane(host) {
        "Control plane does not run sandboxes".into()
    } else {
        "Runtime: fleet-host".into()
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
    if bump {
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

    fn assert_no_pause(text: &str) {
        let lower = text.to_ascii_lowercase();
        assert!(
            !lower.contains("pause") && !lower.contains("resume"),
            "hold copy must not say Pause/Resume: {text}"
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

    #[test]
    fn offline_does_not_repeat_status_as_a_sentence() {
        let mut host = sample_host();
        host.status = "offline".into();
        assert!(not_taking_reason(&host).is_none());
        assert!(extra_status_pills(&host).is_empty());
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
        sort_hosts_for_list(&mut active);
        assert_eq!(active[0].id, plane.id);
    }

    #[test]
    fn extra_pills_show_pending_apply_and_outdated() {
        let mut host = sample_host();
        host.settings_gen = 2;
        host.settings_ack_gen = 1;
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert!(settings_awaiting_ack(&host));
        assert_eq!(extra_status_pills(&host), vec!["pending apply", "outdated"]);

        host.status = "pending".into();
        assert_eq!(extra_status_pills(&host), vec!["outdated"]);

        host.status = "online".into();
        host.settings_gen = 1;
        host.settings_pending = false;
        host.software = serde_json::from_str(r#"{"updating":true}"#).expect("software");
        assert!(extra_status_pills(&host).is_empty());
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
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":"2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"}}"#,
        )
        .expect("software");
        assert_eq!(software_line(&host), "unknown");
        assert!(!software_line(&host).contains("2a971d69"));
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

    #[test]
    fn sort_hosts_plane_then_status_then_name() {
        fn host(id: &str, kind: &str, status: &str, name: &str) -> HostPublic {
            let mut h = sample_host();
            h.id = id.into();
            h.kind = kind.into();
            h.status = status.into();
            h.name = name.into();
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
        sort_hosts_for_list(&mut active);
        let ids: Vec<&str> = active.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["plane", "oa", "oz", "p", "e", "off", "u"]);
        assert_eq!(status_rank("online"), 0);
        assert_eq!(status_rank("pending"), 1);
        assert_eq!(status_rank("error"), 2);
        assert_eq!(status_rank("offline"), 3);
        assert_eq!(status_rank("disabled"), 4);
        assert_eq!(status_rank("mystery"), 5);
        assert!(cmp_hosts_for_list(&plane, &online_a).is_lt());
        assert!(cmp_hosts_for_list(&online_a, &online_z).is_lt());
        assert!(cmp_hosts_for_list(&online_z, &pending).is_lt());
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

    #[test]
    fn runtime_line_is_host_not_plane_backend() {
        let mut host = sample_host();
        assert_eq!(runtime_line(&host), "Runtime: fleet-host");
        host.kind = "control_plane".into();
        assert_eq!(runtime_line(&host), "Control plane does not run sandboxes");
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
