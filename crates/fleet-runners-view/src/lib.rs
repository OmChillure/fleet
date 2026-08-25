//! Host-testable self-hosted runner card formatters (extracted from fleet-ui).
//!
//! Pure predicates and display helpers. No Dioxus, no web-sys, no wasm.
//! Cloud / Partners placeholder copy lives in [`placeholders`].

pub mod placeholders;
pub mod routes;
mod types;

pub use types::{
    CreateHostResponse, HostDesiredSoftware, HostGpu, HostHardware, HostPatch, HostPublic,
    HostSoftware, PackageInventory,
};

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

/// Status vs the package this plane serves for that OS. Never mix a git
/// bake with a file hash on the card — those are not the same id.
pub fn software_flag(host: &HostPublic) -> Option<&'static str> {
    if host.software.as_ref().is_some_and(|s| s.updating) {
        return Some("updating");
    }
    if host.auto_update
        && needs_software_update(host)
        && host.current_jobs == 0
        && software_build_id(host).is_some()
    {
        return Some("updating");
    }
    if host.outdated || host.software_status.as_deref() == Some("behind") {
        return Some("outdated");
    }
    if host.software_status.as_deref() == Some("current") {
        if software_build_id(host).is_some() {
            return Some("up to date");
        }
        // Matches served files, but that package has no bake id (never restaged).
        return Some("old package");
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

/// Compile-time tree-id baked into fleet-host. Never a file hash, never a
/// manifest stamp that was written without rebuilding the PE.
pub fn software_build_id(host: &HostPublic) -> Option<&str> {
    host.software
        .as_ref()
        .and_then(|s| s.git_sha.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Tree-id parts. Dirty suffix (`HEAD-<12 hex>`) is part of the id.
fn split_build_id(s: &str) -> (&str, Option<&str>) {
    let s = s.trim();
    match s.split_once('-') {
        Some((head, rest)) if !head.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit()) => {
            (head, Some(rest))
        }
        _ => (s, None),
    }
}

fn head_same(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.len() < 8 || b.len() < 8 {
        return false;
    }
    let a = a.to_ascii_lowercase();
    let b = b.to_ascii_lowercase();
    a.starts_with(&b) || b.starts_with(&a)
}

fn sha_same(a: &str, b: &str) -> bool {
    let (a_head, a_dirty) = split_build_id(a);
    let (b_head, b_dirty) = split_build_id(b);
    if a_head.is_empty() || b_head.is_empty() || !head_same(a_head, b_head) {
        return false;
    }
    match (a_dirty, b_dirty) {
        (None, None) => true,
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackageReport {
    /// Current *served* runner package id (same 12-char short as the cards).
    pub current_build: String,
    /// Platforms whose staged sha is that runner build. Empty still renders
    /// "Serving this build:" so a blank list is obvious.
    pub serving: String,
    /// Each entry is `sha · plat · plat` (or `no build id · …`).
    pub older: Vec<String>,
}

#[derive(Clone, Copy)]
struct PkgRef<'a> {
    plat: &'a str,
    sha: Option<&'a str>,
    staged: Option<&'a str>,
}

fn collect_pkgs(inv: &PackageInventory) -> Vec<PkgRef<'_>> {
    inv.packages
        .iter()
        .filter_map(|p| {
            let plat = p
                .platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            Some(PkgRef {
                plat,
                sha: p
                    .git_sha
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
                staged: p
                    .staged_at
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
            })
        })
        .collect()
}

/// Newest staged runner sha, else the majority sha. Ignores the plane.
fn pick_current_runner_sha<'a>(pkgs: &[PkgRef<'a>]) -> Option<&'a str> {
    let stamped: Vec<&str> = pkgs.iter().filter_map(|p| p.sha).collect();
    if stamped.is_empty() {
        return None;
    }
    if stamped.iter().all(|s| sha_same(s, stamped[0])) {
        return stamped.into_iter().max_by_key(|s| s.len());
    }
    if pkgs.iter().any(|p| p.sha.is_some() && p.staged.is_some()) {
        return pkgs
            .iter()
            .filter(|p| p.sha.is_some())
            .max_by(|a, b| {
                a.staged
                    .unwrap_or("")
                    .cmp(b.staged.unwrap_or(""))
                    .then_with(|| a.sha.unwrap_or("").len().cmp(&b.sha.unwrap_or("").len()))
            })
            .and_then(|p| p.sha);
    }
    let mut groups: Vec<(&str, usize)> = Vec::new();
    for s in &stamped {
        if let Some((_, n)) = groups.iter_mut().find(|(g, _)| sha_same(g, s)) {
            *n += 1;
        } else {
            groups.push((*s, 1));
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.len().cmp(&a.0.len())));
    Some(groups[0].0)
}

pub fn package_report(inv: &PackageInventory) -> Option<PackageReport> {
    let pkgs = collect_pkgs(inv);
    if pkgs.is_empty() {
        return None;
    }

    let current = pick_current_runner_sha(&pkgs);
    let mut serving = Vec::new();
    let mut older_groups: Vec<(String, Vec<String>)> = Vec::new();
    for p in &pkgs {
        if let (Some(cur), Some(sha)) = (current, p.sha) {
            if sha_same(cur, sha) {
                serving.push(p.plat.to_string());
                continue;
            }
        }
        let key = p.sha.map(short_sha).unwrap_or("no build id").to_string();
        if let Some((_, plats)) = older_groups.iter_mut().find(|(k, _)| *k == key) {
            plats.push(p.plat.to_string());
        } else {
            older_groups.push((key, vec![p.plat.to_string()]));
        }
    }

    Some(PackageReport {
        current_build: current.map(short_sha).unwrap_or("unknown").to_string(),
        serving: serving.join(" · "),
        older: older_groups
            .into_iter()
            .map(|(key, plats)| format!("{key} · {}", plats.join(" · ")))
            .collect(),
    })
}

fn short_sha(sha: &str) -> &str {
    let s = sha.trim();
    if s.len() > 12 { &s[..12] } else { s }
}

pub fn software_line(host: &HostPublic) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(sha) = software_build_id(host) {
        parts.push(short_sha(sha).to_string());
    }
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
            Some(p) => format!("Running the {p} package this control plane is serving."),
            None => "Running the package this control plane is serving for this OS.".into(),
        },
        Some("outdated") => match plat {
            Some(p) => format!("Not running the {p} package this control plane is serving."),
            None => "Not running the package this control plane is serving for this OS.".into(),
        },
        Some("updating") => "Installing the package this control plane is serving.".into(),
        Some("old package") => {
            "This box matches the files we still serve for this OS, but that package has no build id. Restage this platform.".into()
        }
        _ => "This box has not reported a version we can compare.".into(),
    }
}

pub fn display_name(host: &HostPublic) -> String {
    let n = host.name.trim();
    if n.is_empty() {
        "Unnamed worker".into()
    } else {
        n.to_string()
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
    host.outdated
        || host.software_status.as_deref() == Some("behind")
        || (host.software_status.as_deref() == Some("current") && software_build_id(host).is_none())
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
    if is_control_plane(host) || auto_update || !needs_software_update(host) {
        return None;
    }
    if update_requested(host) {
        Some("Update requested")
    } else {
        Some("Update now")
    }
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

/// How this runner executes. Isolation is process < container < vm, not SANDBOX_BACKEND.
pub fn runtime_line(host: &HostPublic, sandbox_backend: &str) -> String {
    if is_control_plane(host) {
        let _ = sandbox_backend;
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

    fn sample_pkg(platform: &str, sha: Option<&str>) -> HostDesiredSoftware {
        HostDesiredSoftware {
            platform: Some(platform.into()),
            git_sha: sha.map(str::to_string),
            ..HostDesiredSoftware::default()
        }
    }

    fn sample_pkg_staged(
        platform: &str,
        sha: Option<&str>,
        staged_at: &str,
    ) -> HostDesiredSoftware {
        let mut p = sample_pkg(platform, sha);
        p.staged_at = Some(staged_at.into());
        p
    }

    #[test]
    fn package_report_ignores_control_plane_for_runner_outdated() {
        let inv = PackageInventory {
            plane_git_sha: Some("9fe136dbda7e7621b644edfffd06bf535584b250".into()),
            packages: vec![
                sample_pkg("linux-x86_64", Some("db33239c786cabcdef")),
                sample_pkg("windows-x86_64", Some("db33239c786cabcdef")),
            ],
        };
        let report = package_report(&inv).expect("report");
        assert_eq!(report.current_build, "db33239c786c");
        assert_eq!(report.serving, "linux-x86_64 · windows-x86_64");
        assert!(report.older.is_empty());
    }

    #[test]
    fn package_report_uses_same_short_sha_as_cards() {
        let inv = PackageInventory {
            packages: vec![sample_pkg(
                "linux-x86_64",
                Some("db33239c786cabcdef0123456789-7db877c8ee4c"),
            )],
            ..PackageInventory::default()
        };
        let report = package_report(&inv).expect("report");
        assert_eq!(report.current_build, "db33239c786c");
        assert!(!report.current_build.contains('-'));
    }

    #[test]
    fn package_report_splits_runner_packages_by_their_own_sha() {
        let inv = PackageInventory {
            plane_git_sha: Some("aaaaaaaaaaaaaaaa".into()),
            packages: vec![
                sample_pkg_staged(
                    "linux-x86_64",
                    Some("bbbbbbbbbbbbbbbb"),
                    "2026-08-20T12:00:00Z",
                ),
                sample_pkg_staged("freebsd-amd64", Some("bbbbbbbb"), "2026-08-20T12:00:00Z"),
                sample_pkg_staged(
                    "windows-x86_64",
                    Some("ccccccccccccdddd"),
                    "2026-08-01T00:00:00Z",
                ),
                sample_pkg("linux-armv7", None),
            ],
        };
        let report = package_report(&inv).expect("report");
        assert_eq!(report.current_build, "bbbbbbbbbbbb");
        assert_eq!(report.serving, "linux-x86_64 · freebsd-amd64");
        assert_eq!(
            report.older,
            vec![
                "cccccccccccc · windows-x86_64".to_string(),
                "no build id · linux-armv7".to_string(),
            ]
        );
    }

    #[test]
    fn package_report_all_current_omits_outdated() {
        let inv = PackageInventory {
            plane_git_sha: Some("planeplaneplane".into()),
            packages: vec![
                sample_pkg("linux-x86_64", Some("aaaaaaaa")),
                sample_pkg("windows-x86_64", Some("aaaaaaaa")),
            ],
        };
        let report = package_report(&inv).expect("report");
        assert_eq!(report.current_build, "aaaaaaaa");
        assert_eq!(report.serving, "linux-x86_64 · windows-x86_64");
        assert!(report.older.is_empty());
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
        host.software_status = Some("current".into());
        assert_eq!(
            software_line(&host),
            "9d239a509055 · linux-x86_64 · up to date"
        );
        assert!(!software_line(&host).contains("2a971d69"));
    }

    #[test]
    fn software_current_without_bake_is_old_package() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":"2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"}}"#,
        )
        .expect("software");
        host.software_status = Some("current".into());
        assert_eq!(software_line(&host), "windows-x86_64 · old package");
        assert_eq!(software_flag(&host), Some("old package"));
    }

    #[test]
    fn software_outdated_keeps_build_sha() {
        let mut host = sample_host();
        host.platform = Some("windows-x86_64".into());
        host.software =
            serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef"}"#).expect("software");
        // Default auto_update is true; an idle behind box then reads as
        // "updating". This case is auto-update off, so the flag stays outdated
        // and the bake id stays on the line.
        host.auto_update = false;
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert_eq!(software_flag(&host), Some("outdated"));
        assert_eq!(
            software_line(&host),
            "9d239a509055 · windows-x86_64 · outdated"
        );
        assert!(!software_line(&host).contains("updating"));
    }

    #[test]
    fn software_auto_update_idle_behind_is_updating() {
        let mut host = sample_host();
        host.platform = Some("linux-x86_64".into());
        host.software =
            serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef"}"#).expect("software");
        host.auto_update = true;
        host.current_jobs = 0;
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert_eq!(software_flag(&host), Some("updating"));
        assert_eq!(
            software_line(&host),
            "9d239a509055 · linux-x86_64 · updating"
        );

        host.current_jobs = 1;
        assert_eq!(software_flag(&host), Some("outdated"));
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
        host.software =
            serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef"}"#).expect("software");
        host.auto_update = false;
        host.software_status = Some("current".into());
        assert!(
            software_title(&host).contains("linux-x86_64"),
            "{}",
            software_title(&host)
        );

        host.software_status = Some("behind".into());
        host.outdated = true;
        assert!(
            software_title(&host).contains("Not running the linux-x86_64 package"),
            "{}",
            software_title(&host)
        );

        host.software = serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef","updating":true}"#)
            .expect("software");
        assert_eq!(
            software_title(&host),
            "Installing the package this control plane is serving."
        );
    }

    #[test]
    fn package_report_empty_serving_when_all_lack_build_id() {
        let inv = PackageInventory {
            packages: vec![
                sample_pkg("linux-x86_64", None),
                sample_pkg("windows-x86_64", None),
            ],
            ..PackageInventory::default()
        };
        let report = package_report(&inv).expect("report");
        assert_eq!(report.current_build, "unknown");
        assert!(report.serving.is_empty(), "blank serving must still render");
        assert_eq!(
            report.older,
            vec!["no build id · linux-x86_64 · windows-x86_64".to_string()]
        );
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
    fn runtime_line_is_sandbox_backend_not_isolation_modes() {
        let mut host = sample_host();
        assert_eq!(
            runtime_line(&host, "docker"),
            "Runtime: fleet-host"
        );
        host.kind = "control_plane".into();
        assert_eq!(
            runtime_line(&host, "process"),
            "Control plane does not run sandboxes"
        );
        assert_eq!(
            runtime_line(&host, ""),
            "Control plane does not run sandboxes"
        );
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
    fn software_title_covers_old_package_and_unknown() {
        let mut host = sample_host();
        assert_eq!(
            software_title(&host),
            "This box has not reported a version we can compare."
        );

        host.software_status = Some("current".into());
        host.platform = Some("linux-x86_64".into());
        assert_eq!(
            software_title(&host),
            "This box matches the files we still serve for this OS, but that package has no build id. Restage this platform."
        );

        host.software =
            serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef"}"#).expect("software");
        assert!(
            software_title(&host).contains("Running the linux-x86_64 package"),
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
        assert!(needs_software_update(&host), "current without bake is old");
        host.software =
            serde_json::from_str(r#"{"git_sha":"9d239a509055abcdef"}"#).expect("software");
        assert!(!needs_software_update(&host));

        assert!(package_report(&PackageInventory::default()).is_none());
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
