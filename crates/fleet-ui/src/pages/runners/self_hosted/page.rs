//! Self-hosted runners: pair a machine that phones home (no inbound dial).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{
    create_host, install_commands, list_workers_page, patch_host, revoke_host,
    windows_install_command, CreateHostBody, CreateHostResponse, HostPatch, HostPublic,
    PackageInventory,
};
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;

const POLL_MS: u32 = 10_000;

fn field_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1 text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-muted)]"
}

fn status_pill_class(status: &str) -> &'static str {
    match status {
        "online" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "pending" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        "offline" => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
        "disabled" => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
        "error" => "bg-red-500/15 text-red-400 border-red-500/30",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

fn format_ts(ts: &str) -> String {
    if ts.is_empty() {
        return "—".into();
    }
    ts.get(..19).unwrap_or(ts).replace('T', " ")
}

fn relative_seen(ts: &Option<String>) -> String {
    let Some(raw) = ts.as_ref().filter(|s| !s.is_empty()) else {
        return "never".into();
    };
    let parsed = js_sys::Date::parse(raw);
    if parsed.is_nan() {
        return format_ts(raw);
    }
    let secs = ((js_sys::Date::now() - parsed) / 1000.0).max(0.0) as i64;
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

fn status_label(status: &str) -> &str {
    match status {
        "disabled" => "removed",
        other => other,
    }
}

/// Status vs the package this plane serves for that OS. Never mix a git
/// bake with a file hash on the card — those are not the same id.
fn software_flag(host: &HostPublic) -> Option<&'static str> {
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
fn software_build_id(host: &HostPublic) -> Option<&str> {
    host.software
        .as_ref()
        .and_then(|s| s.git_sha.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn package_line(inv: &PackageInventory) -> Option<String> {
    if inv.packages.is_empty() {
        return None;
    }
    let bits: Vec<String> = inv
        .packages
        .iter()
        .filter_map(|p| {
            let plat = p
                .platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let sha = p
                .git_sha
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            Some(match sha {
                Some(s) => format!("{plat} {}", short_sha(s)),
                None => format!("{plat} no build id"),
            })
        })
        .collect();
    if bits.is_empty() {
        None
    } else {
        Some(format!("Served packages: {}", bits.join(" · ")))
    }
}

fn packages_need_restage(inv: &PackageInventory) -> bool {
    inv.packages.iter().any(|p| {
        p.git_sha
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
            && p.platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some()
    })
}

fn short_sha(sha: &str) -> &str {
    let s = sha.trim();
    if s.len() > 12 {
        &s[..12]
    } else {
        s
    }
}

fn software_line(host: &HostPublic) -> String {
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

fn software_title(host: &HostPublic) -> String {
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

fn display_name(host: &HostPublic) -> String {
    let n = host.name.trim();
    if n.is_empty() {
        "Unnamed worker".into()
    } else {
        n.to_string()
    }
}

fn holding_new_jobs(host: &HostPublic) -> bool {
    host.paused || host.drain
}

fn hold_jobs_reason(host: &HostPublic) -> Option<String> {
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

fn needs_software_update(host: &HostPublic) -> bool {
    host.outdated
        || host.software_status.as_deref() == Some("behind")
        || (host.software_status.as_deref() == Some("current") && software_build_id(host).is_none())
}

fn accept_jobs_label(holding: bool) -> &'static str {
    if holding {
        "Take jobs again"
    } else {
        "Stop taking new jobs"
    }
}

fn accept_jobs_title(holding: bool) -> &'static str {
    if holding {
        "This worker will take new jobs again. Jobs already running were never stopped."
    } else {
        "Does not stop jobs already running here. New jobs go to another worker, or run on the control plane if none are free."
    }
}

fn not_taking_reason(host: &HostPublic) -> Option<String> {
    match host.status.as_str() {
        "disabled" => Some("Removed — will not take jobs.".into()),
        "pending" => Some("Waiting to pair.".into()),
        "offline" => Some("Offline — fleet-host is not heartbeating.".into()),
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

fn hint_is_loopback(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

fn replace_host(list: &mut Vec<HostPublic>, updated: HostPublic) {
    if let Some(slot) = list.iter_mut().find(|h| h.id == updated.id) {
        *slot = updated;
    }
}

fn status_rank(status: &str) -> u8 {
    match status {
        "online" => 0,
        "pending" => 1,
        "error" => 2,
        "offline" => 3,
        "disabled" => 4,
        _ => 5,
    }
}

fn is_removed(host: &HostPublic) -> bool {
    host.status == "disabled"
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
fn in_removed_list(host: &HostPublic, all: &[HostPublic]) -> bool {
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

fn worker_id_label(host: &HostPublic) -> String {
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

fn job_cap(host: &HostPublic) -> i32 {
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
    let mut s = raw.split_whitespace().collect::<Vec<_>>().join(" ");
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

fn fmt_gpu(g: &super::api::HostGpu) -> Option<String> {
    let name = g.name.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    match g.vram_bytes.filter(|b| *b > 0) {
        Some(b) => Some(format!("{name} {}", fmt_gib(b))),
        None => Some(name.to_string()),
    }
}

fn specs_line(host: &HostPublic) -> Option<String> {
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

#[component]
pub fn SelfHosted(workspace_id: String) -> Element {
    let mut loading = use_signal(|| true);
    let mut creating = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut hosts = use_signal(Vec::<HostPublic>::new);
    let mut pair_result = use_signal(|| None::<CreateHostResponse>);
    let mut show_removed = use_signal(|| false);
    let mut packages = use_signal(PackageInventory::default);

    // Do not read `hosts` / `loading` here — a write-triggered effect loops
    // and Firefox OOMs when the first card mounts.
    let ws_poll = workspace_id.clone();
    use_effect(move || {
        let ws = ws_poll.clone();
        spawn(async move {
            let mut first = true;
            loop {
                match list_workers_page(&ws).await {
                    Ok((list, inv)) => {
                        hosts.set(list);
                        packages.set(inv);
                        error.set(None);
                    }
                    Err(e) => {
                        if first {
                            error.set(Some(e.clone()));
                            show_toast().error(e);
                        }
                    }
                }
                first = false;
                loading.set(false);
                TimeoutFuture::new(POLL_MS).await;
            }
        });
    });

    let ws_create = workspace_id.clone();
    let on_add = move |_| {
        if creating() {
            return;
        }
        creating.set(true);
        let ws = ws_create.clone();
        spawn(async move {
            match create_host(&ws, &CreateHostBody::default()).await {
                Ok(created) => {
                    let mut list = hosts();
                    if !list.iter().any(|h| h.id == created.host.id) {
                        list.insert(0, created.host.clone());
                        hosts.set(list);
                    }
                    pair_result.set(Some(created));
                    error.set(None);
                    show_toast().success("Worker created. Copy the pair token now.");
                }
                Err(e) => {
                    error.set(Some(e.clone()));
                    show_toast().error(e);
                }
            }
            creating.set(false);
        });
    };

    let all = hosts();
    let mut active: Vec<HostPublic> = all.iter().filter(|h| !is_removed(h)).cloned().collect();
    active.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| a.name.cmp(&b.name))
    });
    let removed: Vec<HostPublic> = all
        .iter()
        .filter(|h| in_removed_list(h, &all))
        .cloned()
        .collect();
    let removed_n = removed.len();

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                        "Self-hosted"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Pair a machine as a self-hosted worker. It phones home; the plane never dials inbound. Jobs run as processes on that machine."
                    }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        "Isolation: Native"
                    }
                    if let Some(line) = package_line(&packages()) {
                        p {
                            class: if packages_need_restage(&packages()) {
                                "mt-2 text-xs text-amber-300/90"
                            } else {
                                "mt-2 text-xs text-[var(--app-muted)]"
                            },
                            "{line}"
                        }
                    }
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                    r#type: "button",
                    disabled: creating() || loading(),
                    onclick: on_add,
                    if creating() { "Adding…" } else { "Add worker" }
                }
            }

            if let Some(created) = pair_result() {
                PairPanel {
                    created,
                    on_dismiss: move |_| pair_result.set(None),
                }
            }

            if loading() {
                p { class: "mt-8 text-sm text-[var(--app-muted)]", "Loading…" }
            } else {
                if let Some(err) = error() {
                    p { class: "mt-4 text-sm text-red-400", "{err}" }
                }

                if active.is_empty() && removed_n == 0 {
                    div {
                        class: "mt-6 rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-8 text-center",
                        p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                            "No workers yet"
                        }
                        p { class: "mt-2 text-sm text-[var(--app-muted)]",
                            "Add a worker, then paste the install command in a terminal on that machine."
                        }
                    }
                } else {
                    if active.is_empty() {
                        p { class: "mt-6 text-sm text-[var(--app-muted)]",
                            "No active workers."
                        }
                    } else {
                        div { class: "mt-6 flex flex-col gap-3",
                            for host in active {
                                HostCard {
                                    key: "{host.id}",
                                    workspace_id: workspace_id.clone(),
                                    host,
                                    on_updated: move |updated: HostPublic| {
                                        let mut list = hosts();
                                        replace_host(&mut list, updated);
                                        hosts.set(list);
                                    },
                                    on_revoked: move |id: String| {
                                        let mut list = hosts();
                                        if let Some(slot) = list.iter_mut().find(|h| h.id == id) {
                                            slot.status = "disabled".into();
                                        }
                                        hosts.set(list);
                                    },
                                }
                            }
                        }
                    }
                    if removed_n > 0 {
                        button {
                            class: "mt-4 text-xs font-medium text-[var(--app-muted)] hover:text-[var(--app-fg)]",
                            r#type: "button",
                            onclick: move |_| show_removed.set(!show_removed()),
                            if show_removed() {
                                {format!("Hide removed ({removed_n})")}
                            } else {
                                {format!("Show removed ({removed_n})")}
                            }
                        }
                        if show_removed() {
                            div { class: "mt-3 flex flex-col gap-3",
                                for host in removed {
                                    HostCard {
                                        key: "rm-{host.id}",
                                        workspace_id: workspace_id.clone(),
                                        host,
                                        on_updated: move |updated: HostPublic| {
                                            let mut list = hosts();
                                            replace_host(&mut list, updated);
                                            hosts.set(list);
                                        },
                                        on_revoked: move |id: String| {
                                            let mut list = hosts();
                                            if let Some(slot) = list.iter_mut().find(|h| h.id == id)
                                            {
                                                slot.status = "disabled".into();
                                            }
                                            hosts.set(list);
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn PairPanel(created: CreateHostResponse, on_dismiss: EventHandler<MouseEvent>) -> Element {
    let unix = install_commands(&created);
    let windows = windows_install_command(&created);
    let expires = format_ts(&created.pair_expires_at);
    let token = created.pair_token.clone();
    let unix_copy = unix.clone();
    let windows_copy = windows.clone();
    let loopback_hint = hint_is_loopback(&created.install_hint) || hint_is_loopback(&unix);

    rsx! {
        section {
            class: "mt-6 rounded-2xl border border-amber-500/30 bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "Pair this worker"
            }
            p { class: "mt-1 text-sm text-[var(--app-fg)]",
                "On the machine, open a terminal and paste the command. It installs fleet-host, fleet-worker, and fleet-driver, then pairs and starts."
            }
            p { class: "mt-1 text-xs text-amber-300/90",
                "This token is shown once and expires {expires}."
            }
            if loopback_hint {
                p { class: "mt-2 text-xs text-amber-300/90",
                    "This command uses a loopback URL. Another computer needs a LAN or public control-plane URL."
                }
            }

            label { class: "mt-4 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Linux, macOS, FreeBSD"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Open a terminal and paste:"
            }
            div { class: "mt-1 flex flex-wrap items-start gap-2",
                pre {
                    class: "m-0 min-w-0 flex-1 overflow-x-auto whitespace-pre-wrap rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-xs text-[var(--app-fg)]",
                    "{unix}"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: move |_| {
                        if copy_text(&unix_copy) {
                            show_toast().success("Command copied.");
                        } else {
                            show_toast().error("Copy failed. Select the command and copy it yourself.");
                        }
                    },
                    "Copy"
                }
            }

            label { class: "mt-4 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Windows"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Open PowerShell and paste:"
            }
            div { class: "mt-1 flex flex-wrap items-start gap-2",
                pre {
                    class: "m-0 min-w-0 flex-1 overflow-x-auto whitespace-pre-wrap rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-xs text-[var(--app-fg)]",
                    "{windows}"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: move |_| {
                        if copy_text(&windows_copy) {
                            show_toast().success("Command copied.");
                        } else {
                            show_toast().error("Copy failed. Select the command and copy it yourself.");
                        }
                    },
                    "Copy"
                }
            }

            label { class: "mt-4 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Pair token"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Already in the command. Shown once if you pair by hand."
            }
            div { class: "mt-1 flex flex-wrap items-start gap-2",
                pre {
                    class: "m-0 min-w-0 flex-1 overflow-x-auto rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-xs text-[var(--app-fg)]",
                    "{created.pair_token}"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: move |_| {
                        if copy_text(&token) {
                            show_toast().success("Token copied.");
                        } else {
                            show_toast().error("Copy failed. Select the token and copy it yourself.");
                        }
                    },
                    "Copy"
                }
            }

            button {
                class: "mt-4 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                r#type: "button",
                onclick: move |e| on_dismiss.call(e),
                "Done"
            }
        }
    }
}

fn btn_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50"
}

#[component]
fn HostCard(
    workspace_id: String,
    host: HostPublic,
    on_updated: EventHandler<HostPublic>,
    on_revoked: EventHandler<String>,
) -> Element {
    let mut busy = use_signal(|| false);
    let mut confirm_remove = use_signal(|| false);
    let mut renaming = use_signal(|| false);
    let mut name_draft = use_signal(|| host.name.clone());
    let revoked = host.status == "disabled";
    let pill = status_pill_class(&host.status);
    let last_seen = relative_seen(&host.last_seen_at);
    let title = display_name(&host);
    let why = not_taking_reason(&host);
    let holding = holding_new_jobs(&host);
    let hostname = host.hostname.clone().unwrap_or_default();
    let platform = host.platform.clone().unwrap_or_default();
    let last_error = host.last_error.clone().unwrap_or_default();
    let show_error = !last_error.is_empty()
        && why
            .as_ref()
            .map(|w| !w.contains(&last_error))
            .unwrap_or(true);

    rsx! {
        article { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div { class: "min-w-0 flex flex-wrap items-center gap-2",
                    if renaming() && !revoked {
                        input {
                            class: "{field_class()} min-w-[10rem]",
                            value: "{name_draft}",
                            autofocus: true,
                            oninput: move |e| name_draft.set(e.value()),
                            onkeydown: {
                                let workspace_id = workspace_id.clone();
                                let host = host.clone();
                                move |e: Event<KeyboardData>| {
                                    let key = e.key();
                                    if key == Key::Escape {
                                        name_draft.set(host.name.clone());
                                        renaming.set(false);
                                    } else if key == Key::Enter {
                                        let next = name_draft().trim().to_string();
                                        if next.is_empty() || next == host.name {
                                            renaming.set(false);
                                            return;
                                        }
                                        if busy() {
                                            return;
                                        }
                                        busy.set(true);
                                        let ws = workspace_id.clone();
                                        let current = host.clone();
                                        let patch = HostPatch {
                                            name: Some(next),
                                            ..HostPatch::default()
                                        };
                                        spawn(async move {
                                            match patch_host(&ws, &current.id, &current, &patch)
                                                .await
                                            {
                                                Ok(updated) => on_updated.call(updated),
                                                Err(e) => show_toast().error(e),
                                            }
                                            busy.set(false);
                                            renaming.set(false);
                                        });
                                    }
                                }
                            },
                        }
                    } else {
                        button {
                            class: "m-0 border-0 bg-transparent p-0 text-left text-sm font-semibold text-[var(--app-fg)] hover:underline disabled:no-underline disabled:opacity-70",
                            r#type: "button",
                            disabled: revoked,
                            title: "Rename",
                            onclick: {
                                let name = host.name.clone();
                                move |_| {
                                    name_draft.set(name.clone());
                                    renaming.set(true);
                                }
                            },
                            "{title}"
                        }
                    }
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{status_label(&host.status)}"
                    }
                }
                span { class: "text-xs text-[var(--app-muted)]",
                    "last seen {last_seen}"
                }
                span {
                    class: "text-xs text-[var(--app-muted)]",
                    title: "Stable worker id. Same machine keeps this id after remove + re-pair.",
                    "worker {worker_id_label(&host)}"
                }
            }

            if host.status != "pending" && host.status != "disabled" {
                p {
                    class: "mt-1 text-xs text-[var(--app-muted)]",
                    title: "{software_title(&host)}",
                    "software "
                    if software_flag(&host) == Some("outdated")
                        || software_flag(&host) == Some("old package")
                    {
                        span { class: "text-amber-400",
                            "{software_line(&host)}"
                        }
                    } else {
                        "{software_line(&host)}"
                    }
                }
            }

            if !hostname.is_empty() || !platform.is_empty() {
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    if !hostname.is_empty() {
                        "{hostname}"
                    }
                    if !hostname.is_empty() && !platform.is_empty() {
                        " · "
                    }
                    if !platform.is_empty() {
                        "{platform}"
                    }
                }
            }
            if let Some(specs) = specs_line(&host) {
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "{specs}"
                }
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "jobs {host.current_jobs} / {job_cap(&host)}"
                if let Some(pct) = host.hardware.as_ref().and_then(|h| h.cpu_used_pct) {
                    " · cpu {pct}%"
                }
                if let Some(pct) = host.hardware.as_ref().and_then(|h| h.mem_used_pct) {
                    " · mem {pct}%"
                }
                if let Some(pct) = host.hardware.as_ref().and_then(|h| h.gpu_used_pct) {
                    " · gpu {pct}%"
                }
            }

            if let Some(reason) = why {
                p { class: "mt-2 text-xs text-amber-300/90",
                    "{reason}"
                }
            }
            if show_error {
                p { class: "mt-2 text-xs text-red-400", "{last_error}" }
            }

            if !revoked {
                div { class: "mt-3 flex flex-wrap items-center gap-x-4 gap-y-2",
                    button {
                        class: "{btn_class()}",
                        r#type: "button",
                        disabled: busy(),
                        title: "{accept_jobs_title(holding)}",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            let host = host.clone();
                            let next_hold = !holding;
                            move |_| {
                                if busy() {
                                    return;
                                }
                                busy.set(true);
                                let ws = workspace_id.clone();
                                let current = host.clone();
                                let patch = HostPatch {
                                    paused: Some(next_hold),
                                    drain: if next_hold { None } else { Some(false) },
                                    ..HostPatch::default()
                                };
                                spawn(async move {
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => {
                                            on_updated.call(updated);
                                            if next_hold {
                                                show_toast().success(
                                                    "Not taking new jobs. Running jobs keep going.",
                                                );
                                            } else {
                                                show_toast().success(
                                                    "This worker will take new jobs again.",
                                                );
                                            }
                                        }
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "{accept_jobs_label(holding)}"
                    }
                    label {
                        class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                        title: "When on, this worker pulls the served package by itself. Turn off to wait for Update now.",
                        input {
                            r#type: "checkbox",
                            checked: host.auto_update,
                            disabled: busy(),
                            onchange: {
                                let workspace_id = workspace_id.clone();
                                let host = host.clone();
                                move |e| {
                                    if busy() {
                                        return;
                                    }
                                    let on = e.checked();
                                    if on == host.auto_update {
                                        return;
                                    }
                                    busy.set(true);
                                    let ws = workspace_id.clone();
                                    let current = host.clone();
                                    let patch = HostPatch {
                                        auto_update: Some(on),
                                        ..HostPatch::default()
                                    };
                                    spawn(async move {
                                        match patch_host(&ws, &current.id, &current, &patch).await
                                        {
                                            Ok(updated) => on_updated.call(updated),
                                            Err(err) => show_toast().error(err),
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                        }
                        "Auto-update"
                    }
                    if !host.auto_update && needs_software_update(&host) {
                        button {
                            class: "{btn_class()}",
                            r#type: "button",
                            disabled: busy() || host.update_requested_at.as_deref().is_some_and(|s| !s.trim().is_empty()),
                            title: "Ask this worker to apply the served package on its next heartbeat. Does not stop running jobs.",
                            onclick: {
                                let workspace_id = workspace_id.clone();
                                let host = host.clone();
                                move |_| {
                                    if busy() {
                                        return;
                                    }
                                    busy.set(true);
                                    let ws = workspace_id.clone();
                                    let current = host.clone();
                                    let patch = HostPatch {
                                        request_update: Some(true),
                                        ..HostPatch::default()
                                    };
                                    spawn(async move {
                                        match patch_host(&ws, &current.id, &current, &patch).await
                                        {
                                            Ok(updated) => {
                                                on_updated.call(updated);
                                                show_toast().success(
                                                    "Update requested. The worker applies it on the next heartbeat if it can.",
                                                );
                                            }
                                            Err(err) => show_toast().error(err),
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                            if host.update_requested_at.as_deref().is_some_and(|s| !s.trim().is_empty()) {
                                "Update requested"
                            } else {
                                "Update now"
                            }
                        }
                    }
                    label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                        "Max jobs at once"
                        input {
                            class: "{field_class()} w-16",
                            r#type: "number",
                            min: "1",
                            max: "256",
                            value: "{host.max_concurrent}",
                            disabled: busy(),
                            onchange: {
                                let workspace_id = workspace_id.clone();
                                let host = host.clone();
                                move |e| {
                                    if busy() {
                                        return;
                                    }
                                    let Ok(n) = e.value().parse::<i32>() else {
                                        return;
                                    };
                                    let n = n.clamp(1, 256);
                                    if n == host.max_concurrent {
                                        return;
                                    }
                                    busy.set(true);
                                    let ws = workspace_id.clone();
                                    let current = host.clone();
                                    let patch = HostPatch {
                                        max_concurrent: Some(n),
                                        ..HostPatch::default()
                                    };
                                    spawn(async move {
                                        match patch_host(&ws, &current.id, &current, &patch).await
                                        {
                                            Ok(updated) => on_updated.call(updated),
                                            Err(e) => show_toast().error(e),
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                        }
                    }
                }
                if !holding {
                    p { class: "mt-1.5 text-xs text-[var(--app-muted)]",
                        "Does not stop jobs already running on this machine."
                    }
                }

                div { class: "mt-3 border-t border-[var(--app-border)] pt-3",
                    if confirm_remove() {
                        span { class: "inline-flex flex-wrap items-center gap-2 text-xs",
                            span { class: "text-[var(--app-muted)]",
                                "Remove this worker? It must be paired again to come back."
                            }
                            button {
                                class: "rounded-lg border border-red-500/30 px-2.5 py-1 text-xs font-semibold text-red-300 hover:bg-red-500/15 disabled:opacity-50",
                                r#type: "button",
                                disabled: busy(),
                                onclick: {
                                    let workspace_id = workspace_id.clone();
                                    let host_id = host.id.clone();
                                    move |_| {
                                        if busy() {
                                            return;
                                        }
                                        busy.set(true);
                                        let ws = workspace_id.clone();
                                        let id = host_id.clone();
                                        spawn(async move {
                                            match revoke_host(&ws, &id).await {
                                                Ok(()) => {
                                                    on_revoked.call(id);
                                                    show_toast().success("Worker removed.");
                                                }
                                                Err(e) => show_toast().error(e),
                                            }
                                            busy.set(false);
                                            confirm_remove.set(false);
                                        });
                                    }
                                },
                                "Confirm"
                            }
                            button {
                                class: "{btn_class()}",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| confirm_remove.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        button {
                            class: "rounded-lg border border-red-500/30 px-2.5 py-1 text-xs font-semibold text-red-300 hover:bg-red-500/15 disabled:opacity-50",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| confirm_remove.set(true),
                            "Remove worker"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        host.hardware = Some(super::super::api::HostHardware {
            cpu_model: Some("AMD Ryzen 7 5700G with Radeon Graphics".into()),
            cpu_cores: Some(16),
            mem_total_bytes: Some(32 * 1_073_741_824),
            mem_speed_mts: Some(3200),
            mem_type: Some("DDR4".into()),
            gpus: Some(vec![super::super::api::HostGpu {
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
        host.hardware = Some(super::super::api::HostHardware {
            cpu_model: Some("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz".into()),
            cpu_cores: Some(8),
            gpus: Some(vec![super::super::api::HostGpu {
                name: Some("Intel UHD Graphics".into()),
                vram_bytes: None,
                used_pct: Some(4),
            }]),
            ..super::super::api::HostHardware::default()
        });
        let line = specs_line(&host).expect("specs");
        assert!(line.contains("Intel UHD Graphics"), "{line}");
        assert!(line.contains("i7-10610U"), "{line}");
    }

    fn specs_line_drops_missing_optionals() {
        let mut host = sample_host();
        host.hardware = Some(super::super::api::HostHardware {
            cpu_cores: Some(8),
            mem_total_bytes: Some(8 * 1_073_741_824),
            ..super::super::api::HostHardware::default()
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
        host.outdated = true;
        host.software_status = Some("behind".into());
        assert_eq!(
            software_line(&host),
            "9d239a509055 · windows-x86_64 · outdated"
        );
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
    fn software_never_uses_file_hash_as_build_id() {
        let mut host = sample_host();
        host.software = serde_json::from_str(
            r#"{"bins":{"fleet-host":"2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"}}"#,
        )
        .expect("software");
        assert_eq!(software_line(&host), "unknown");
        assert!(!software_line(&host).contains("2a971d69"));
    }
}
